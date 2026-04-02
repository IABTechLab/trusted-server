//! Partner registry — `PartnerRecord` schema and `PartnerStore` operations.
//!
//! Each partner (SSP, DSP, identity vendor) is stored as a JSON record in
//! the Fastly KV Store keyed by `partner_id`. A secondary index
//! `apikey:{sha256_hex}` provides O(1) API key lookups for batch sync auth.

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
/// Created via `POST /_ts/admin/partners/register`. Used by pixel sync, batch
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_pull_token: Option<String>,
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

    if record.ts_pull_token.as_deref().unwrap_or("").is_empty() {
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

/// Computes the SHA-256 hex digest of an API key.
#[must_use]
pub fn hash_api_key(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Wraps a Fastly KV Store for partner registry operations.
///
/// Partner records are keyed by `partner_id`. A secondary index
/// `apikey:{sha256_hex}` maps API key hashes to partner IDs for
/// O(1) auth lookups during batch sync.
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
                if key.starts_with(APIKEY_INDEX_PREFIX) {
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

    /// Writes or updates a partner record and maintains the API key index.
    ///
    /// Returns `true` if this was a new partner (create), `false` if an
    /// existing partner was updated.
    ///
    /// Index maintenance order:
    /// 1. Read existing `apikey:` index value for rollback
    /// 2. Write new `apikey:` index
    /// 3. Write primary record
    /// 4. Delete old `apikey:` index (if key rotated)
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
        // key rotation.
        if record.api_key_hash != hash {
            log::warn!(
                "API key hash mismatch for partner '{}' (stale index after key rotation)",
                record.id,
            );
            return Ok(None);
        }

        Ok(Some(record))
    }

    /// Verifies an API key against the stored hash for a given partner.
    ///
    /// Uses SHA-256 hashing and constant-time comparison to prevent
    /// timing attacks.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] if the partner lookup fails.
    pub fn verify_api_key(
        &self,
        partner_id: &str,
        api_key: &str,
    ) -> Result<bool, Report<TrustedServerError>> {
        let record = match self.get(partner_id)? {
            Some(r) => r,
            None => return Ok(false),
        };

        let incoming_hash = hash_api_key(api_key);
        let stored_bytes = record.api_key_hash.as_bytes();
        let incoming_bytes = incoming_hash.as_bytes();

        Ok(stored_bytes.ct_eq(incoming_bytes).into())
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
    }
}
