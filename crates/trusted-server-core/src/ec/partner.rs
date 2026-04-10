//! Partner registry — `PartnerRecord` schema and `PartnerStore` operations.
//!
//! Each partner (SSP, DSP, identity vendor) is stored as a JSON record in
//! the Fastly KV Store keyed by `partner_id`. Three secondary indexes exist:
//!
//! - `apikey:{sha256_hex}` — maps API key hashes to partner IDs for O(1)
//!   auth lookups during batch sync.
//! - `_pull_enabled` — JSON array of partner IDs with `pull_sync_enabled:
//!   true`, enabling O(1+N) reads on the pull sync hot path instead of a
//!   full partner scan.
//! - `_fp_signal_enabled` — JSON array of partner configs for partners
//!   with at least one `fp_signal_cookie_names` entry, enabling O(1+N)
//!   reads for first-party signal collection config without a full
//!   partner scan.

use std::{collections::HashSet, sync::OnceLock};

use error_stack::{Report, ResultExt};
use fastly::kv_store::KVStore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::TrustedServerError;

/// Regex pattern for valid partner IDs.
/// Lowercase alphanumeric, hyphens, underscores; 1-32 characters.
const PARTNER_ID_PATTERN: &str = r"^[a-z0-9_-]{1,32}$";

/// Reserved partner IDs that would collide with managed `X-ts-*` headers.
const RESERVED_PARTNER_IDS: &[&str] = &[
    "ec",
    "eids",
    "ec-consent",
    "eids-truncated",
    "synthetic",
    "ts",
    "version",
    "env",
];

/// Prefix for the API key hash secondary index keys.
const APIKEY_INDEX_PREFIX: &str = "apikey:";

/// Key for the pull-enabled partner secondary index.
///
/// Stores a JSON array of partner IDs that have `pull_sync_enabled: true`.
/// Updated on every `upsert()` so that `pull_enabled_partners()` can read
/// a single index key instead of listing/scanning all partners.
const PULL_ENABLED_INDEX_KEY: &str = "_pull_enabled";

/// Key for the FP-signal-enabled partner secondary index.
///
/// Stores a JSON array of [`super::fp_signals::FpSignalPartnerConfig`] for
/// partners with at least one `fp_signal_cookie_names` entry. Updated on
/// every `upsert()` so that `fp_signal_configs()` can read a single key
/// instead of scanning all partners.
const FP_SIGNAL_INDEX_KEY: &str = "_fp_signal_enabled";

/// Cached compiled regex for partner ID validation.
static PARTNER_ID_REGEX: OnceLock<Result<Regex, String>> = OnceLock::new();

fn partner_id_regex() -> Result<&'static Regex, String> {
    PARTNER_ID_REGEX
        .get_or_init(|| {
            Regex::new(PARTNER_ID_PATTERN)
                .map_err(|e| format!("internal error compiling partner ID regex: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// A registered partner configuration stored in the partner KV store.
///
/// Created via `POST /_ts/admin/v1/partners/register`. Used by pixel sync, batch
/// sync, pull sync, and auction bidstream decoration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartnerRecord {
    /// Unique partner identifier. Must match [`PARTNER_ID_PATTERN`] and
    /// not be in [`RESERVED_PARTNER_IDS`]. Used to build `X-ts-<id>`
    /// response headers.
    pub id: String,
    /// Human-readable partner name.
    pub name: String,
    /// Exact hostnames allowed as `return` URL domains in pixel sync.
    pub allowed_return_domains: Vec<String>,
    /// SHA-256 hex of the partner's API key. Plaintext is never stored.
    pub api_key_hash: String,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    pub bidstream_enabled: bool,
    /// `OpenRTB` `source.domain` for EID entries (e.g. `"liveramp.com"`).
    pub source_domain: String,
    /// `OpenRTB` `atype` value (typically 3).
    pub openrtb_atype: u8,
    /// Max pixel sync writes per EC hash per partner per hour.
    ///
    /// **Note:** Fastly rate counters only expose 60-second windows, so the
    /// effective enforcement is `sync_rate_limit / 60` per minute. This can
    /// create bursty behavior for low limits (e.g. a limit of 60 allows
    /// 1 sync per 60 seconds, not a smooth 1/sec). See `FastlyRateLimiter`
    /// in `sync_pixel.rs` for details.
    pub sync_rate_limit: u32,
    /// Max batch sync API requests per partner per minute.
    pub batch_rate_limit: u32,
    /// Whether server-to-server pull sync is enabled for this partner.
    pub pull_sync_enabled: bool,
    /// URL to call for pull sync. Required when `pull_sync_enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_sync_url: Option<String>,
    /// Allowlist of domains TS may call for this partner's pull sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_sync_allowed_domains: Vec<String>,
    /// Seconds between pull sync refreshes (default 86400).
    pub pull_sync_ttl_sec: u64,
    /// Max pull sync calls per EC hash per partner per hour.
    pub pull_sync_rate_limit: u32,
    /// Outbound bearer token for pull sync requests.
    ///
    /// Stored in plaintext (unlike `api_key_hash`, which is SHA-256 hashed).
    /// This is intentional: `ts_pull_token` is an *outbound* credential that
    /// TS sends to the partner's pull sync endpoint, so it must be readable
    /// at runtime. `api_key_hash` is an *inbound* credential that partners
    /// send to us, so it only needs hash verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_pull_token: Option<String>,
    /// First-party cookie names that may carry this partner's UID.
    /// Checked in order; first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fp_signal_cookie_names: Vec<String>,
    /// Dot-notation JSON path to extract the UID from a JSON cookie
    /// value. When `None`, the raw cookie value is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp_signal_json_path: Option<String>,
    /// Minimum seconds between re-collection writes for this partner.
    /// Defaults to 86400 (24 hours).
    #[serde(default = "PartnerRecord::default_fp_signal_ttl_sec")]
    pub fp_signal_ttl_sec: u64,
}

impl PartnerRecord {
    fn default_fp_signal_ttl_sec() -> u64 {
        86400
    }
}

/// Validates a partner ID format and checks against reserved names.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_partner_id(id: &str) -> Result<(), String> {
    let re = partner_id_regex()?;
    if !re.is_match(id) {
        return Err(format!(
            "partner ID must match {PARTNER_ID_PATTERN}, got: '{id}'"
        ));
    }
    if RESERVED_PARTNER_IDS.contains(&id) {
        return Err(format!("partner ID '{id}' is reserved"));
    }
    Ok(())
}

/// Validates pull sync configuration consistency.
///
/// When `pull_sync_enabled` is true, both `pull_sync_url` and
/// `ts_pull_token` must be present, and the URL's hostname must
/// appear in `pull_sync_allowed_domains`.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_pull_sync_config(record: &PartnerRecord) -> Result<(), String> {
    if !record.pull_sync_enabled {
        return Ok(());
    }

    let url_str = record.pull_sync_url.as_deref().unwrap_or("");
    if url_str.is_empty() {
        return Err(
            "pull_sync_url and ts_pull_token are required when pull_sync_enabled is true"
                .to_owned(),
        );
    }

    if record
        .ts_pull_token
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return Err(
            "pull_sync_url and ts_pull_token are required when pull_sync_enabled is true"
                .to_owned(),
        );
    }

    // Validate that the pull sync URL uses HTTPS (bearer tokens must not
    // travel over plaintext).
    let parsed =
        url::Url::parse(url_str).map_err(|e| format!("pull_sync_url is not a valid URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err(format!(
            "pull_sync_url must use HTTPS, got scheme '{}'",
            parsed.scheme()
        ));
    }

    // Validate that the pull sync URL hostname is in the allowed domains.
    let host = parsed
        .host_str()
        .ok_or("pull_sync_url has no hostname")?
        .trim_end_matches('.')
        .to_ascii_lowercase();

    let allowed: HashSet<String> = record
        .pull_sync_allowed_domains
        .iter()
        .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
        .collect();

    if !allowed.contains(&host) {
        return Err("pull_sync_url domain must be in pull_sync_allowed_domains".to_owned());
    }

    Ok(())
}

/// Validates first-party signal configuration fields on a [`PartnerRecord`].
///
/// If `fp_signal_cookie_names` is empty, all FP signal fields are ignored
/// and validation always succeeds. When cookie names are present the
/// following rules apply:
///
/// - At most 5 cookie names.
/// - Each cookie name must be non-empty, contain only ASCII characters, and
///   must not contain `;` or `=` (reserved in the Cookie header spec).
/// - If `fp_signal_json_path` is `Some`, it must be non-empty, contain only
///   alphanumeric characters, `.`, or `_`, and have at most 4 dot-separated
///   segments.
/// - `fp_signal_ttl_sec` must be between 60 and 604800 (7 days) inclusive.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_fp_signal_config(record: &PartnerRecord) -> Result<(), String> {
    if record.fp_signal_cookie_names.is_empty() {
        return Ok(());
    }

    const MAX_COOKIE_NAMES: usize = 5;
    if record.fp_signal_cookie_names.len() > MAX_COOKIE_NAMES {
        return Err(format!(
            "fp_signal_cookie_names must have at most {MAX_COOKIE_NAMES} entries, got {}",
            record.fp_signal_cookie_names.len()
        ));
    }

    const MAX_COOKIE_NAME_LENGTH: usize = 128;
    for name in &record.fp_signal_cookie_names {
        if name.is_empty() {
            return Err("fp_signal_cookie_names entries must not be empty".to_owned());
        }
        if name.len() > MAX_COOKIE_NAME_LENGTH {
            return Err(format!(
                "fp_signal_cookie_names entry must be at most {MAX_COOKIE_NAME_LENGTH} characters, got {}",
                name.len()
            ));
        }
        if !name.is_ascii() {
            return Err(format!(
                "fp_signal_cookie_names entry '{name}' must contain only ASCII characters"
            ));
        }
        if name.contains(';') || name.contains('=') {
            return Err(format!(
                "fp_signal_cookie_names entry '{name}' must not contain ';' or '='"
            ));
        }
    }

    if let Some(path) = &record.fp_signal_json_path {
        if path.is_empty() {
            return Err("fp_signal_json_path must not be empty when present".to_owned());
        }
        if !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        {
            return Err(format!(
                "fp_signal_json_path '{path}' must contain only alphanumeric characters, '.', or '_'"
            ));
        }
        let segment_count = path.split('.').count();
        const MAX_SEGMENTS: usize = 4;
        if segment_count > MAX_SEGMENTS {
            return Err(format!(
                "fp_signal_json_path '{path}' must have at most {MAX_SEGMENTS} dot-separated segments, got {segment_count}"
            ));
        }
    }

    const MIN_TTL: u64 = 60;
    const MAX_TTL: u64 = 604_800;
    if record.fp_signal_ttl_sec < MIN_TTL || record.fp_signal_ttl_sec > MAX_TTL {
        return Err(format!(
            "fp_signal_ttl_sec must be between {MIN_TTL} and {MAX_TTL}, got {}",
            record.fp_signal_ttl_sec
        ));
    }

    Ok(())
}

/// Computes the SHA-256 hex digest of an API key.
#[must_use]
pub fn hash_api_key(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Wraps a Fastly KV Store for partner registry operations.
///
/// Partner records are keyed by `partner_id`. Three secondary indexes
/// optimize hot-path operations:
///
/// - `apikey:{sha256_hex}` maps API key hashes to partner IDs for
///   O(1) auth lookups during batch sync.
/// - `_pull_enabled` stores a JSON array of partner IDs with
///   `pull_sync_enabled: true` for O(1+N) reads during pull sync dispatch.
/// - `_fp_signal_enabled` stores a JSON array of partner configs for
///   partners with at least one `fp_signal_cookie_names` entry for
///   O(1+N) reads during first-party signal collection.
pub struct PartnerStore {
    store_name: String,
}

impl PartnerStore {
    /// Creates a new partner store backed by the named KV store.
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
                message: "Failed to open partner store".to_owned(),
            })?
            .ok_or_else(|| {
                Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: "Partner store not found".to_owned(),
                })
            })
    }

    /// Reads a partner record by ID.
    ///
    /// Returns `Ok(None)` when the partner is not registered.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store or deserialization failure.
    pub fn get(
        &self,
        partner_id: &str,
    ) -> Result<Option<PartnerRecord>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut response = match store.lookup(partner_id) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read partner '{partner_id}'"),
                    }),
                );
            }
        };

        let body_bytes = response.take_body_bytes();
        let record: PartnerRecord =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to deserialize partner '{partner_id}'"),
            })?;

        Ok(Some(record))
    }

    /// Lists all registered partner records.
    ///
    /// Scans the partner KV store and returns records for non-index keys.
    /// Secondary index entries (e.g. `apikey:*`) are skipped.
    ///
    /// **Scaling note:** This performs O(N) KV reads where N is the number
    /// of registered partners. Called by `dispatch_pull_sync` on every
    /// organic request post-send. For large partner counts, consider
    /// caching the result or maintaining a summary key. The
    /// `pull_sync_concurrency` setting bounds downstream dispatch but
    /// does not reduce the enumeration cost.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on list, lookup, or
    /// deserialization failure.
    pub fn list_registered(&self) -> Result<Vec<PartnerRecord>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut records = Vec::new();

        for page in store.build_list().limit(1000).iter() {
            let page = page.change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: "Failed to list partner keys".to_owned(),
            })?;

            for key in page.keys() {
                if key.starts_with(APIKEY_INDEX_PREFIX)
                    || key == PULL_ENABLED_INDEX_KEY
                    || key == FP_SIGNAL_INDEX_KEY
                {
                    continue;
                }

                let mut response = match store.lookup(key) {
                    Ok(resp) => resp,
                    Err(fastly::kv_store::KVStoreError::ItemNotFound) => continue,
                    Err(err) => {
                        return Err(
                            Report::new(err).change_context(TrustedServerError::KvStore {
                                store_name: self.store_name.clone(),
                                message: format!("Failed to read partner '{key}' while listing"),
                            }),
                        );
                    }
                };

                let body_bytes = response.take_body_bytes();
                let record = serde_json::from_slice::<PartnerRecord>(&body_bytes).change_context(
                    TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to deserialize partner '{key}' while listing"),
                    },
                )?;

                records.push(record);
            }
        }

        Ok(records)
    }

    /// Returns pull-enabled partners via the `_pull_enabled` secondary index.
    ///
    /// Performs 1 index read + N partner reads (where N = pull-enabled count)
    /// instead of scanning all partners. Falls back to [`list_registered`]
    /// with client-side filtering when the index is missing or unreadable,
    /// ensuring correctness even before the first `upsert()` writes the index.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store or deserialization failure.
    pub fn pull_enabled_partners(&self) -> Result<Vec<PartnerRecord>, Report<TrustedServerError>> {
        let store = self.open_store()?;

        // Read the secondary index.
        let index_body = match store.lookup(PULL_ENABLED_INDEX_KEY) {
            Ok(mut resp) => resp.take_body_bytes(),
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => {
                // Index not yet written — fall back to full scan.
                log::debug!("Pull-enabled index missing, falling back to list_registered");
                return self
                    .list_registered()
                    .map(|v| v.into_iter().filter(|p| p.pull_sync_enabled).collect());
            }
            Err(err) => {
                // Index unreadable — fall back to full scan rather than failing.
                log::warn!(
                    "Failed to read pull-enabled index, falling back to list_registered: {err:?}"
                );
                return self
                    .list_registered()
                    .map(|v| v.into_iter().filter(|p| p.pull_sync_enabled).collect());
            }
        };

        let partner_ids: Vec<String> =
            serde_json::from_slice(&index_body).change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: "Failed to deserialize pull-enabled index".to_owned(),
            })?;

        let mut records = Vec::with_capacity(partner_ids.len());
        for partner_id in &partner_ids {
            match self.get(partner_id)? {
                Some(record) if record.pull_sync_enabled => {
                    records.push(record);
                }
                Some(_) => {
                    // Index is stale — partner is no longer pull-enabled.
                    // This is self-healing: the next `upsert()` will fix the index.
                    log::debug!(
                        "Pull-enabled index references partner '{}' which is no longer pull-enabled",
                        partner_id
                    );
                }
                None => {
                    log::debug!(
                        "Pull-enabled index references non-existent partner '{}'",
                        partner_id
                    );
                }
            }
        }

        Ok(records)
    }

    /// Writes or updates a partner record and maintains secondary indexes.
    ///
    /// Returns `true` if this was a new partner (create), `false` if an
    /// existing partner was updated.
    ///
    /// Index maintenance order:
    /// 1. Read existing `apikey:` index value for rollback
    /// 2. Write new `apikey:` index
    /// 3. Write primary record
    /// 4. Delete old `apikey:` index (if key rotated)
    /// 5. Update `_pull_enabled` secondary index (best-effort)
    ///
    /// Writes are still **not fully atomic**, but this order ensures
    /// registration does not return success after a failed index write and
    /// performs best-effort rollback when primary write fails.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store failure.
    pub fn upsert(&self, record: &PartnerRecord) -> Result<bool, Report<TrustedServerError>> {
        let store = self.open_store()?;

        // Read existing record to detect API key rotation.
        let existing = match store.lookup(&record.id) {
            Ok(mut resp) => {
                let bytes = resp.take_body_bytes();
                serde_json::from_slice::<PartnerRecord>(&bytes).ok()
            }
            Err(_) => None,
        };

        let is_create = existing.is_none();
        let old_api_key_hash = existing.as_ref().map(|r| r.api_key_hash.clone());

        let index_key = format!("{APIKEY_INDEX_PREFIX}{}", record.api_key_hash);
        let previous_index_partner_id = self.read_index_partner_id(&store, &index_key)?;

        // Guard against API key hash collision: reject if the index already
        // points to a *different* partner. This prevents silently reassigning
        // another partner's auth mapping.
        if let Some(ref existing_id) = previous_index_partner_id {
            if existing_id != &record.id {
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!(
                        "API key hash already assigned to partner '{existing_id}', \
                         cannot assign to '{}'",
                        record.id
                    ),
                }));
            }
        }

        // 1. Write new API key index.
        store
            .build_insert()
            .execute(&index_key, record.id.as_str())
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!(
                    "Failed to write API key index for partner '{}' (hash '{}')",
                    record.id, record.api_key_hash
                ),
            })?;

        // 2. Write primary record.
        let body = serde_json::to_string(record).change_context(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!("Failed to serialize partner '{}'", record.id),
        })?;

        if let Err(err) = store.build_insert().execute(&record.id, body) {
            self.restore_previous_index_mapping(
                &store,
                &index_key,
                previous_index_partner_id.as_deref(),
                &record.id,
            );
            return Err(
                Report::new(err).change_context(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!("Failed to write partner '{}'", record.id),
                }),
            );
        }

        // 3. Delete old API key index if key rotated.
        if let Some(ref old_hash) = old_api_key_hash {
            if *old_hash != record.api_key_hash {
                let old_key = format!("{APIKEY_INDEX_PREFIX}{old_hash}");
                if let Err(err) = store.delete(&old_key) {
                    log::warn!(
                        "Failed to delete old API key index for partner '{}': {err:?}",
                        record.id,
                    );
                }
            }
        }

        // 4. Update _pull_enabled secondary index (best-effort).
        //    This is the last step so a failure here doesn't affect the
        //    primary registration. `pull_enabled_partners()` falls back to
        //    `list_registered()` if the index is missing or stale.
        self.update_pull_enabled_index(&store, &record.id, record.pull_sync_enabled);

        // 5. Update _fp_signal_enabled secondary index (best-effort).
        self.update_fp_signal_index(&store, record);

        Ok(is_create)
    }

    fn read_index_partner_id(
        &self,
        store: &KVStore,
        index_key: &str,
    ) -> Result<Option<String>, Report<TrustedServerError>> {
        let mut response = match store.lookup(index_key) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!(
                            "Failed to read existing API key index before upsert ('{index_key}')"
                        ),
                    }),
                );
            }
        };

        let partner_id = String::from_utf8(response.take_body_bytes()).map_err(|_| {
            Report::new(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!(
                    "Existing API key index value is not valid UTF-8 before upsert ('{index_key}')"
                ),
            })
        })?;

        Ok(Some(partner_id))
    }

    /// Best-effort update of the `_pull_enabled` secondary index.
    ///
    /// Reads the current index, adds or removes the partner ID, and writes
    /// it back. All errors are logged and swallowed — the primary record
    /// write has already succeeded.
    ///
    /// **Race condition:** This performs a read-modify-write without CAS.
    /// Concurrent partner registrations can overwrite each other's index
    /// updates (last write wins). The index is self-healing: any lost entry
    /// will be re-added on the next `upsert()` for that partner, and
    /// `pull_enabled_partners()` falls back to `list_registered()` when the
    /// index is missing or stale.
    fn update_pull_enabled_index(
        &self,
        store: &KVStore,
        partner_id: &str,
        pull_sync_enabled: bool,
    ) {
        // Read existing index (or start with empty list).
        let mut ids: Vec<String> = match store.lookup(PULL_ENABLED_INDEX_KEY) {
            Ok(mut resp) => {
                let bytes = resp.take_body_bytes();
                serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                    log::warn!("Failed to deserialize pull-enabled index, starting fresh: {err}");
                    Vec::new()
                })
            }
            Err(_) => Vec::new(),
        };

        let had_id = ids.iter().any(|id| id == partner_id);

        if pull_sync_enabled && !had_id {
            ids.push(partner_id.to_owned());
        } else if !pull_sync_enabled && had_id {
            ids.retain(|id| id != partner_id);
        } else {
            // No change needed.
            return;
        }

        let body = match serde_json::to_string(&ids) {
            Ok(b) => b,
            Err(err) => {
                log::warn!(
                    "Failed to serialize pull-enabled index after updating partner '{}': {err}",
                    partner_id
                );
                return;
            }
        };

        if let Err(err) = store.build_insert().execute(PULL_ENABLED_INDEX_KEY, body) {
            log::warn!(
                "Failed to write pull-enabled index after updating partner '{}': {err:?}",
                partner_id
            );
        }
    }

    /// Best-effort update of the `_fp_signal_enabled` secondary index.
    ///
    /// Reads the current index, replaces the entry for `record.id`, and
    /// writes it back. If `fp_signal_cookie_names` is empty the partner is
    /// removed from the index. All errors are logged and swallowed — the
    /// primary record write has already succeeded.
    fn update_fp_signal_index(&self, store: &KVStore, record: &PartnerRecord) {
        // Read existing index (or start with empty list).
        let mut configs: Vec<super::fp_signals::FpSignalPartnerConfig> =
            match store.lookup(FP_SIGNAL_INDEX_KEY) {
                Ok(mut resp) => {
                    let bytes = resp.take_body_bytes();
                    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                        log::warn!("Failed to deserialize fp-signal index, starting fresh: {err}");
                        Vec::new()
                    })
                }
                Err(_) => Vec::new(),
            };

        // Remove any existing entry for this partner.
        configs.retain(|c| c.partner_id != record.id);

        // Re-add if partner has cookie names configured.
        if !record.fp_signal_cookie_names.is_empty() {
            configs.push(super::fp_signals::FpSignalPartnerConfig {
                partner_id: record.id.clone(),
                cookie_names: record.fp_signal_cookie_names.clone(),
                json_path: record.fp_signal_json_path.clone(),
                ttl_sec: record.fp_signal_ttl_sec,
            });
        }

        let body = match serde_json::to_string(&configs) {
            Ok(b) => b,
            Err(err) => {
                log::warn!(
                    "Failed to serialize fp-signal index after updating partner '{}': {err}",
                    record.id
                );
                return;
            }
        };

        if let Err(err) = store.build_insert().execute(FP_SIGNAL_INDEX_KEY, body) {
            log::warn!(
                "Failed to write fp-signal index after updating partner '{}': {err:?}",
                record.id
            );
        }
    }

    /// Returns FP-signal-enabled partner configs via the `_fp_signal_enabled`
    /// secondary index.
    ///
    /// Returns an empty [`Vec`] when the index is missing or corrupt rather
    /// than propagating an error, matching the best-effort semantics of the
    /// index itself.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] if the store cannot be opened.
    pub fn fp_signal_configs(
        &self,
    ) -> Result<Vec<super::fp_signals::FpSignalPartnerConfig>, Report<TrustedServerError>> {
        let store = self.open_store()?;

        let index_body = match store.lookup(FP_SIGNAL_INDEX_KEY) {
            Ok(mut resp) => resp.take_body_bytes(),
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(Vec::new()),
            Err(err) => {
                log::warn!("Failed to read fp-signal index: {err:?}");
                return Ok(Vec::new());
            }
        };

        let configs = serde_json::from_slice(&index_body).unwrap_or_else(|err| {
            log::warn!("Failed to deserialize fp-signal index, returning empty: {err}");
            Vec::new()
        });

        Ok(configs)
    }

    fn restore_previous_index_mapping(
        &self,
        store: &KVStore,
        index_key: &str,
        previous_partner_id: Option<&str>,
        partner_id: &str,
    ) {
        if let Some(previous_partner_id) = previous_partner_id {
            if previous_partner_id == partner_id {
                return;
            }

            if let Err(err) = store.build_insert().execute(index_key, previous_partner_id) {
                log::warn!(
                    "Failed to restore previous API key index mapping after write failure for partner '{}': {err:?}",
                    partner_id,
                );
            }
            return;
        }

        match store.delete(index_key) {
            Ok(()) | Err(fastly::kv_store::KVStoreError::ItemNotFound) => {}
            Err(err) => {
                log::warn!(
                    "Failed to roll back API key index after write failure for partner '{}': {err:?}",
                    partner_id,
                );
            }
        }
    }

    /// Looks up a partner by API key hash using the `apikey:` secondary index.
    ///
    /// After resolving the index to a partner ID, re-verifies that the
    /// stored `api_key_hash` matches the lookup hash (guards against stale
    /// index entries from key rotation).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store failure.
    pub fn find_by_api_key_hash(
        &self,
        hash: &str,
    ) -> Result<Option<PartnerRecord>, Report<TrustedServerError>> {
        let store = self.open_store()?;

        // Look up the secondary index.
        let index_key = format!("{APIKEY_INDEX_PREFIX}{hash}");
        let mut index_response = match store.lookup(&index_key) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read API key index for hash '{hash}'"),
                    }),
                );
            }
        };

        let partner_id = String::from_utf8(index_response.take_body_bytes()).map_err(|_| {
            Report::new(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("API key index value for hash '{hash}' is not valid UTF-8"),
            })
        })?;

        // Fetch the actual partner record.
        let record = match self.get(&partner_id)? {
            Some(r) => r,
            None => {
                // Stale index — partner was deleted.
                log::warn!(
                    "API key index points to non-existent partner '{partner_id}' (stale index)"
                );
                return Ok(None);
            }
        };

        // Verify the stored hash matches — guards against stale index from
        // key rotation. Uses constant-time comparison to prevent timing attacks.
        if !bool::from(record.api_key_hash.as_bytes().ct_eq(hash.as_bytes())) {
            log::warn!(
                "API key hash mismatch for partner '{}' (stale index after key rotation)",
                record.id,
            );
            return Ok(None);
        }

        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partner_record_serialization_roundtrip() {
        let record = PartnerRecord {
            id: "ssp_x".to_owned(),
            name: "SSP Example".to_owned(),
            allowed_return_domains: vec!["sync.example-ssp.com".to_owned()],
            api_key_hash: hash_api_key("test-api-key"),
            bidstream_enabled: true,
            source_domain: "example-ssp.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };

        let json = serde_json::to_string(&record).expect("should serialize");
        let deserialized: PartnerRecord = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized, record);
    }

    #[test]
    fn hash_api_key_is_deterministic() {
        let h1 = hash_api_key("my-secret-key");
        let h2 = hash_api_key("my-secret-key");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64, "should be 64-char hex SHA-256");
    }

    #[test]
    fn hash_api_key_differs_for_different_keys() {
        let h1 = hash_api_key("key-a");
        let h2 = hash_api_key("key-b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn validate_partner_id_accepts_valid() {
        assert!(validate_partner_id("ssp_x").is_ok());
        assert!(validate_partner_id("liveramp").is_ok());
        assert!(validate_partner_id("a-b_c-1").is_ok());
        assert!(validate_partner_id("a").is_ok());
    }

    #[test]
    fn validate_partner_id_rejects_uppercase() {
        let err = validate_partner_id("SSP_X").unwrap_err();
        assert!(
            err.contains("must match"),
            "should reject uppercase, got: {err}"
        );
    }

    #[test]
    fn validate_partner_id_rejects_too_long() {
        let long = "a".repeat(33);
        let err = validate_partner_id(&long).unwrap_err();
        assert!(
            err.contains("must match"),
            "should reject >32 chars, got: {err}"
        );
    }

    #[test]
    fn validate_partner_id_rejects_empty() {
        let err = validate_partner_id("").unwrap_err();
        assert!(
            err.contains("must match"),
            "should reject empty, got: {err}"
        );
    }

    #[test]
    fn validate_partner_id_rejects_reserved() {
        for reserved in RESERVED_PARTNER_IDS {
            let err = validate_partner_id(reserved).unwrap_err();
            assert!(
                err.contains("reserved"),
                "should reject '{reserved}', got: {err}"
            );
        }
    }

    #[test]
    fn validate_partner_id_rejects_special_chars() {
        assert!(validate_partner_id("ssp.x").is_err(), "should reject dots");
        assert!(
            validate_partner_id("ssp x").is_err(),
            "should reject spaces"
        );
        assert!(
            validate_partner_id("ssp/x").is_err(),
            "should reject slashes"
        );
    }

    #[test]
    fn validate_pull_sync_ok_when_disabled() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        assert!(validate_pull_sync_config(&record).is_ok());
    }

    #[test]
    fn validate_pull_sync_rejects_missing_url() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: Some("token".to_owned()),
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        let err = validate_pull_sync_config(&record).unwrap_err();
        assert!(err.contains("pull_sync_url"), "got: {err}");
    }

    #[test]
    fn validate_pull_sync_rejects_missing_token() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.test.com/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.test.com".to_owned()],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        let err = validate_pull_sync_config(&record).unwrap_err();
        assert!(err.contains("ts_pull_token"), "got: {err}");
    }

    #[test]
    fn validate_pull_sync_rejects_http_scheme() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("http://sync.test.com/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.test.com".to_owned()],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: Some("token".to_owned()),
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        let err = validate_pull_sync_config(&record).unwrap_err();
        assert!(
            err.contains("HTTPS"),
            "should reject HTTP scheme, got: {err}"
        );
    }

    #[test]
    fn validate_pull_sync_rejects_url_not_in_allowed_domains() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://evil.com/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.test.com".to_owned()],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: Some("token".to_owned()),
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        let err = validate_pull_sync_config(&record).unwrap_err();
        assert!(err.contains("pull_sync_allowed_domains"), "got: {err}");
    }

    #[test]
    fn validate_pull_sync_accepts_valid_config() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.test.com/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.test.com".to_owned()],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: Some("token".to_owned()),
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        assert!(validate_pull_sync_config(&record).is_ok());
    }

    #[test]
    fn optional_fields_omitted_from_json() {
        let record = PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        };
        let json = serde_json::to_string(&record).expect("should serialize");
        assert!(
            !json.contains("pull_sync_url"),
            "None pull_sync_url should be omitted"
        );
        assert!(
            !json.contains("ts_pull_token"),
            "None ts_pull_token should be omitted"
        );
        assert!(
            !json.contains("pull_sync_allowed_domains"),
            "empty pull_sync_allowed_domains should be omitted"
        );
        assert!(
            !json.contains("fp_signal_cookie_names"),
            "empty fp_signal_cookie_names should be omitted"
        );
        assert!(
            !json.contains("fp_signal_json_path"),
            "None fp_signal_json_path should be omitted"
        );
    }

    fn base_fp_signal_record() -> PartnerRecord {
        PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["uid2_token".to_owned()],
            fp_signal_json_path: Some("advertising_token".to_owned()),
            fp_signal_ttl_sec: 86400,
        }
    }

    #[test]
    fn validate_fp_signal_config_accepts_valid() {
        let record = base_fp_signal_record();
        assert!(
            validate_fp_signal_config(&record).is_ok(),
            "should accept valid FP signal config"
        );
    }

    #[test]
    fn validate_fp_signal_config_ok_when_empty() {
        let mut record = base_fp_signal_record();
        record.fp_signal_cookie_names = vec![];
        assert!(
            validate_fp_signal_config(&record).is_ok(),
            "should accept empty cookie names without validating other fields"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_empty_cookie_name() {
        let mut record = base_fp_signal_record();
        record.fp_signal_cookie_names = vec!["valid_cookie".to_owned(), String::new()];
        let err = validate_fp_signal_config(&record).expect_err("should reject empty cookie name");
        assert!(
            err.contains("must not be empty"),
            "should mention empty entry, got: {err}"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_too_many_cookie_names() {
        let mut record = base_fp_signal_record();
        record.fp_signal_cookie_names = (0..6).map(|i| format!("cookie_{i}")).collect();
        let err = validate_fp_signal_config(&record).expect_err("should reject 6 cookie names");
        assert!(
            err.contains("at most 5"),
            "should mention 5-entry limit, got: {err}"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_cookie_name_with_semicolon() {
        let mut record = base_fp_signal_record();
        record.fp_signal_cookie_names = vec!["bad;name".to_owned()];
        let err = validate_fp_signal_config(&record)
            .expect_err("should reject cookie name with semicolon");
        assert!(
            err.contains("';'"),
            "should mention forbidden character, got: {err}"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_ttl_too_low() {
        let mut record = base_fp_signal_record();
        record.fp_signal_ttl_sec = 10;
        let err = validate_fp_signal_config(&record).expect_err("should reject TTL of 10");
        assert!(
            err.contains("fp_signal_ttl_sec"),
            "should mention ttl field, got: {err}"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_invalid_json_path() {
        let mut record = base_fp_signal_record();
        // "a.b.c.d.e" has 5 segments — exceeds max of 4.
        record.fp_signal_json_path = Some("a.b.c.d.e".to_owned());
        let err = validate_fp_signal_config(&record)
            .expect_err("should reject json path with 5 segments");
        assert!(
            err.contains("at most 4"),
            "should mention 4-segment limit, got: {err}"
        );
    }

    #[test]
    fn validate_fp_signal_rejects_cookie_name_too_long() {
        let mut record = base_fp_signal_record();
        record.fp_signal_cookie_names = vec!["x".repeat(129)];
        let err = validate_fp_signal_config(&record)
            .expect_err("should reject cookie name exceeding 128 chars");
        assert!(
            err.contains("at most 128"),
            "should mention 128-char limit, got: {err}"
        );
    }
}
