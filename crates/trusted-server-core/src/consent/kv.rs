//! KV Store consent persistence.
//!
//! Stores and retrieves consent data from a platform-neutral KV Store, keyed
//! by Synthetic ID. This provides consent continuity for returning users
//! whose browsers may not have consent cookies on every request.
//!
//! # Storage layout
//!
//! Each entry is a single JSON body ([`KvConsentEntry`]) containing raw consent
//! strings, context flags, and a compact fingerprint for change detection.
//!
//! # Change detection
//!
//! Writes only occur when consent signals have actually changed.
//! [`consent_fingerprint`] hashes the raw strings into a compact fingerprint
//! stored inside the body. On the next request, the existing fingerprint is
//! compared before writing.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::jurisdiction::Jurisdiction;
use super::types::{ConsentContext, ConsentSource};

// ---------------------------------------------------------------------------
// KV body (JSON, stored as value)
// ---------------------------------------------------------------------------

/// Consent data stored in the KV Store body.
///
/// Contains the raw consent strings needed to reconstruct a [`ConsentContext`],
/// plus a compact fingerprint used for write-on-change detection.
/// Decoded data (TCF, GPP, US Privacy) is not stored — it is re-decoded on
/// read to avoid stale decoded state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvConsentEntry {
    /// Raw TC String from `euconsent-v2` cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_tc_string: Option<String>,
    /// Raw GPP string from `__gpp` cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_gpp_string: Option<String>,
    /// GPP section IDs (decoded or from `__gpp_sid` cookie).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp_section_ids: Option<Vec<u16>>,
    /// Raw US Privacy string from `us_privacy` cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_us_privacy: Option<String>,
    /// Raw Google Additional Consent (AC) string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_ac_string: Option<String>,

    /// Whether GDPR applies to this request.
    pub gdpr_applies: bool,
    /// Global Privacy Control signal.
    pub gpc: bool,
    /// Serialized jurisdiction (e.g. `"GDPR"`, `"US-CA"`, `"unknown"`).
    pub jurisdiction: String,

    /// When this entry was stored (deciseconds since Unix epoch).
    pub stored_at_ds: u64,

    /// SHA-256 fingerprint (first 16 hex chars) of all raw consent signals.
    ///
    /// Used for write-on-change detection. If the fingerprint of the stored
    /// entry equals the fingerprint of the current request's consent signals,
    /// no write is needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
}

// ---------------------------------------------------------------------------
// Platform-neutral KV operations trait
// ---------------------------------------------------------------------------

/// Synchronous KV operations required for consent persistence.
///
/// Implemented by the platform adapter (e.g., Fastly KV store). Synchronous
/// to remain compatible with the non-async [`super::build_consent_context`]
/// pipeline.
pub trait ConsentKvOps: Send + Sync {
    /// Load a consent entry from the KV store.
    ///
    /// Returns `None` on a cache miss or deserialization failure. Errors are
    /// logged internally and never propagated — KV failures must not break
    /// the request pipeline.
    fn load_entry(&self, key: &str) -> Option<KvConsentEntry>;

    /// Save a consent entry with a time-to-live.
    ///
    /// Errors are logged internally and never propagated.
    fn save_entry_with_ttl(&self, key: &str, entry: &KvConsentEntry, ttl: std::time::Duration);

    /// Delete a consent entry.
    ///
    /// Called when consent is revoked (SSC cookie expiry). Errors are logged
    /// internally and never propagated — KV failures must not break the
    /// request pipeline.
    fn delete_entry(&self, key: &str);
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Builds a [`KvConsentEntry`] from a [`ConsentContext`].
///
/// Captures only the raw strings and contextual flags. Decoded data is
/// intentionally omitted — it will be re-decoded on read. The entry includes
/// a fingerprint for write-on-change detection on subsequent requests.
#[must_use]
pub fn entry_from_context(ctx: &ConsentContext, now_ds: u64) -> KvConsentEntry {
    KvConsentEntry {
        raw_tc_string: ctx.raw_tc_string.clone(),
        raw_gpp_string: ctx.raw_gpp_string.clone(),
        gpp_section_ids: ctx.gpp_section_ids.clone(),
        raw_us_privacy: ctx.raw_us_privacy.clone(),
        raw_ac_string: ctx.raw_ac_string.clone(),
        gdpr_applies: ctx.gdpr_applies,
        gpc: ctx.gpc,
        jurisdiction: ctx.jurisdiction.to_string(),
        stored_at_ds: now_ds,
        fp: Some(consent_fingerprint(ctx)),
    }
}

/// Converts a [`KvConsentEntry`] into [`super::types::RawConsentSignals`]
/// suitable for re-decoding via [`super::build_context_from_signals`].
#[must_use]
pub fn signals_from_entry(entry: &KvConsentEntry) -> super::types::RawConsentSignals {
    super::types::RawConsentSignals {
        raw_tc_string: entry.raw_tc_string.clone(),
        raw_gpp_string: entry.raw_gpp_string.clone(),
        raw_gpp_sid: entry.gpp_section_ids.as_ref().map(|ids| {
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }),
        raw_us_privacy: entry.raw_us_privacy.clone(),
        gpc: entry.gpc,
    }
}

/// Reconstructs a [`ConsentContext`] from a KV Store entry.
///
/// Re-decodes the raw strings to populate structured fields (TCF, GPP, US
/// Privacy). The `source` is set to [`ConsentSource::KvStore`] and the
/// `jurisdiction` is parsed from the stored string representation.
#[must_use]
pub fn context_from_entry(entry: &KvConsentEntry) -> ConsentContext {
    let signals = signals_from_entry(entry);
    let mut ctx = super::build_context_from_signals(&signals);

    // Restore context fields that aren't derived from raw signals.
    ctx.gdpr_applies = entry.gdpr_applies;
    ctx.gpc = entry.gpc;
    ctx.raw_ac_string = entry.raw_ac_string.clone();
    ctx.jurisdiction = parse_jurisdiction(&entry.jurisdiction);
    ctx.source = ConsentSource::KvStore;

    ctx
}

// ---------------------------------------------------------------------------
// Fingerprinting
// ---------------------------------------------------------------------------

/// Computes a compact fingerprint of the consent signals for change detection.
///
/// Returns the first 16 hex characters of a SHA-256 hash computed over all
/// raw consent strings and the GPC flag. This is sufficient for detecting
/// changes without storing full hashes.
#[must_use]
pub fn consent_fingerprint(ctx: &ConsentContext) -> String {
    let mut hasher = Sha256::new();

    // Feed each signal into the hash, separated by a sentinel byte to
    // prevent ambiguity (e.g., None+Some("x") vs Some("x")+None).
    hash_optional(&mut hasher, ctx.raw_tc_string.as_deref());
    hash_optional(&mut hasher, ctx.raw_gpp_string.as_deref());
    hash_optional(&mut hasher, ctx.raw_us_privacy.as_deref());
    hash_optional(&mut hasher, ctx.raw_ac_string.as_deref());
    hasher.update(if ctx.gpc { b"1" } else { b"0" });

    // Include GPP section IDs so SID-only changes trigger a KV write.
    if let Some(sids) = &ctx.gpp_section_ids {
        let mut sorted = sids.clone();
        sorted.sort_unstable();
        for sid in &sorted {
            hasher.update(sid.to_string().as_bytes());
            hasher.update(b"\xFF");
        }
    } else {
        hasher.update(b"\x00");
    }

    let result = hasher.finalize();
    hex::encode(&result[..8]) // 16 hex chars = 8 bytes = 64 bits
}

/// Feeds an optional string into the hasher with sentinel bytes.
fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(s) => {
            hasher.update(b"\x01");
            hasher.update(s.as_bytes());
        }
        None => hasher.update(b"\x00"),
    }
}

/// Parses a jurisdiction string back into a [`Jurisdiction`] enum.
fn parse_jurisdiction(s: &str) -> Jurisdiction {
    match s {
        "GDPR" => Jurisdiction::Gdpr,
        "non-regulated" => Jurisdiction::NonRegulated,
        "unknown" => Jurisdiction::Unknown,
        s if s.starts_with("US-") => Jurisdiction::UsState(s[3..].to_owned()),
        _ => Jurisdiction::Unknown,
    }
}

// ---------------------------------------------------------------------------
// KV Store operations (platform-neutral)
// ---------------------------------------------------------------------------

/// Loads consent data from the KV Store for a given key.
///
/// Returns `Some(ConsentContext)` if a valid entry is found, [`None`] if the
/// key does not exist or deserialization fails. Errors are logged but never
/// propagated — KV Store failures must not break the request pipeline.
///
/// # Arguments
///
/// * `kv` — Platform KV implementation for consent operations.
/// * `key` — The Synthetic ID used as the KV Store key.
#[must_use]
pub fn load_consent(kv: &dyn ConsentKvOps, key: &str) -> Option<ConsentContext> {
    let entry = kv.load_entry(key)?;
    log::info!(
        "Loaded consent from KV store for '{key}' (stored_at_ds={})",
        entry.stored_at_ds
    );
    Some(context_from_entry(&entry))
}

/// Saves consent data to the KV Store, writing only when signals have changed.
///
/// Compares the fingerprint of the current consent signals against the stored
/// body. If they match, the write is skipped. Otherwise, the entry is written
/// with the configured TTL.
///
/// # Arguments
///
/// * `kv` — Platform KV implementation for consent operations.
/// * `key` — The Synthetic ID used as the KV Store key.
/// * `ctx` — The current request's consent context.
/// * `max_age_days` — TTL for the entry, matching `max_consent_age_days`.
pub fn save_consent(kv: &dyn ConsentKvOps, key: &str, ctx: &ConsentContext, max_age_days: u32) {
    if ctx.is_empty() {
        log::debug!("Skipping consent KV write: consent is empty");
        return;
    }
    let new_fp = consent_fingerprint(ctx);
    // Load existing entry once; check fp to skip write when unchanged.
    let existing_fp = kv.load_entry(key).and_then(|e| e.fp);
    if existing_fp.as_deref() == Some(new_fp.as_str()) {
        log::debug!("Consent unchanged for '{key}' (fp={new_fp}), skipping write");
        return;
    }
    let entry = entry_from_context(ctx, super::now_deciseconds());
    let ttl = std::time::Duration::from_secs(u64::from(max_age_days) * 86_400);
    kv.save_entry_with_ttl(key, &entry, ttl);
    log::info!("Saved consent to KV store for '{key}' (fp={new_fp}, ttl={max_age_days}d)");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};

    fn make_test_context() -> ConsentContext {
        ConsentContext {
            raw_tc_string: Some("CPXxGfAPXxGfA".to_owned()),
            raw_gpp_string: Some("DBACNYA~CPXxGfA".to_owned()),
            gpp_section_ids: Some(vec![2, 6]),
            raw_us_privacy: Some("1YNN".to_owned()),
            raw_ac_string: None,
            gdpr_applies: true,
            tcf: None,
            gpp: None,
            us_privacy: None,
            expired: false,
            gpc: false,
            jurisdiction: Jurisdiction::Gdpr,
            source: ConsentSource::Cookie,
        }
    }

    #[test]
    fn entry_roundtrip() {
        let ctx = make_test_context();
        let entry = entry_from_context(&ctx, 1_000_000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        let restored: KvConsentEntry = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(restored.raw_tc_string, ctx.raw_tc_string);
        assert_eq!(restored.raw_gpp_string, ctx.raw_gpp_string);
        assert_eq!(restored.gpp_section_ids, ctx.gpp_section_ids);
        assert_eq!(restored.raw_us_privacy, ctx.raw_us_privacy);
        assert_eq!(restored.gdpr_applies, ctx.gdpr_applies);
        assert_eq!(restored.gpc, ctx.gpc);
        assert_eq!(restored.jurisdiction, "GDPR");
        assert_eq!(restored.stored_at_ds, 1_000_000);
    }

    #[test]
    fn context_roundtrip_via_entry() {
        let original = make_test_context();
        let entry = entry_from_context(&original, 1_000_000);
        let restored = context_from_entry(&entry);

        assert_eq!(restored.raw_tc_string, original.raw_tc_string);
        assert_eq!(restored.raw_gpp_string, original.raw_gpp_string);
        assert_eq!(restored.raw_us_privacy, original.raw_us_privacy);
        assert_eq!(restored.gdpr_applies, original.gdpr_applies);
        assert_eq!(restored.gpc, original.gpc);
        assert_eq!(restored.jurisdiction, original.jurisdiction);
        assert_eq!(restored.source, ConsentSource::KvStore);
    }

    #[test]
    fn fingerprint_deterministic() {
        let ctx = make_test_context();
        let fp1 = consent_fingerprint(&ctx);
        let fp2 = consent_fingerprint(&ctx);
        assert_eq!(fp1, fp2, "fingerprint should be deterministic");
        assert_eq!(fp1.len(), 16, "fingerprint should be 16 hex chars");
    }

    #[test]
    fn fingerprint_changes_with_different_signals() {
        let ctx1 = make_test_context();
        let mut ctx2 = make_test_context();
        ctx2.raw_tc_string = Some("DIFFERENT_TC_STRING".to_owned());

        assert_ne!(
            consent_fingerprint(&ctx1),
            consent_fingerprint(&ctx2),
            "different TC strings should produce different fingerprints"
        );
    }

    #[test]
    fn fingerprint_changes_with_gpc() {
        let mut ctx1 = make_test_context();
        ctx1.gpc = false;
        let mut ctx2 = make_test_context();
        ctx2.gpc = true;

        assert_ne!(
            consent_fingerprint(&ctx1),
            consent_fingerprint(&ctx2),
            "different GPC values should produce different fingerprints"
        );
    }

    #[test]
    fn fingerprint_distinguishes_none_from_empty() {
        let mut ctx_none = make_test_context();
        ctx_none.raw_tc_string = None;
        let mut ctx_empty = make_test_context();
        ctx_empty.raw_tc_string = Some(String::new());

        assert_ne!(
            consent_fingerprint(&ctx_none),
            consent_fingerprint(&ctx_empty),
            "None vs empty string should produce different fingerprints"
        );
    }

    #[test]
    fn signals_from_entry_roundtrip() {
        let ctx = make_test_context();
        let entry = entry_from_context(&ctx, 1_000_000);
        let signals = signals_from_entry(&entry);

        assert_eq!(signals.raw_tc_string, ctx.raw_tc_string);
        assert_eq!(signals.raw_gpp_string, ctx.raw_gpp_string);
        assert_eq!(signals.raw_us_privacy, ctx.raw_us_privacy);
        assert_eq!(signals.gpc, ctx.gpc);
        // gpp_sid is serialized as "2,6" from the section IDs
        assert_eq!(signals.raw_gpp_sid, Some("2,6".to_owned()));
    }

    #[test]
    fn parse_jurisdiction_roundtrip() {
        assert_eq!(parse_jurisdiction("GDPR"), Jurisdiction::Gdpr);
        assert_eq!(
            parse_jurisdiction("US-CA"),
            Jurisdiction::UsState("CA".to_owned())
        );
        assert_eq!(
            parse_jurisdiction("non-regulated"),
            Jurisdiction::NonRegulated
        );
        assert_eq!(parse_jurisdiction("unknown"), Jurisdiction::Unknown);
        assert_eq!(
            parse_jurisdiction("something-else"),
            Jurisdiction::Unknown,
            "unrecognized jurisdiction should default to Unknown"
        );
    }

    #[test]
    fn empty_entry_serializes_compact() {
        let ctx = ConsentContext::default();
        let entry = entry_from_context(&ctx, 0);
        let json = serde_json::to_string(&entry).expect("should serialize");
        // With skip_serializing_if = "Option::is_none", omitted fields keep it small.
        assert!(
            !json.contains("raw_tc_string"),
            "None fields should be omitted from JSON"
        );
    }

    #[test]
    fn entry_preserves_ac_string() {
        let mut ctx = make_test_context();
        ctx.raw_ac_string = Some("2~1234.5678~dv.".to_owned());
        let entry = entry_from_context(&ctx, 0);
        let restored = context_from_entry(&entry);

        assert_eq!(
            restored.raw_ac_string,
            Some("2~1234.5678~dv.".to_owned()),
            "AC string should survive roundtrip"
        );
    }

    // --- ConsentKvOps integration tests using a stub ---

    struct StubKvOps {
        stored: std::sync::Mutex<std::collections::HashMap<String, KvConsentEntry>>,
    }

    impl StubKvOps {
        fn new() -> Self {
            Self {
                stored: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }
    }

    impl ConsentKvOps for StubKvOps {
        fn load_entry(&self, key: &str) -> Option<KvConsentEntry> {
            self.stored
                .lock()
                .expect("should lock stub KV store")
                .get(key)
                .cloned()
        }

        fn save_entry_with_ttl(
            &self,
            key: &str,
            entry: &KvConsentEntry,
            _ttl: std::time::Duration,
        ) {
            self.stored
                .lock()
                .expect("should lock stub KV store")
                .insert(key.to_owned(), entry.clone());
        }

        fn delete_entry(&self, key: &str) {
            self.stored
                .lock()
                .expect("should lock stub KV store")
                .remove(key);
        }
    }

    #[test]
    fn load_consent_returns_none_on_miss() {
        let kv = StubKvOps::new();
        let result = load_consent(&kv, "missing-key");
        assert!(result.is_none(), "should return None on cache miss");
    }

    #[test]
    fn save_and_load_consent_roundtrip() {
        let kv = StubKvOps::new();
        let ctx = make_test_context();
        save_consent(&kv, "user-1", &ctx, 30);
        let loaded = load_consent(&kv, "user-1").expect("should load saved consent");
        assert_eq!(
            loaded.raw_tc_string, ctx.raw_tc_string,
            "should restore raw TC string"
        );
    }

    #[test]
    fn save_consent_skips_write_when_fingerprint_unchanged() {
        let kv = StubKvOps::new();
        let ctx = make_test_context();

        // First write.
        save_consent(&kv, "user-1", &ctx, 30);
        assert_eq!(
            kv.stored.lock().expect("should lock").len(),
            1,
            "should have one entry"
        );

        // Track the stored timestamp to verify no new write happens.
        let stored_ts = kv
            .stored
            .lock()
            .expect("should lock")
            .get("user-1")
            .map(|e| e.stored_at_ds)
            .expect("should find entry after first write");

        // Second write with same context — fingerprint unchanged.
        save_consent(&kv, "user-1", &ctx, 30);
        let ts_after = kv
            .stored
            .lock()
            .expect("should lock")
            .get("user-1")
            .map(|e| e.stored_at_ds)
            .expect("should find entry after second write");

        assert_eq!(
            stored_ts, ts_after,
            "should not overwrite when fingerprint is unchanged"
        );
    }

    #[test]
    fn save_consent_writes_when_fingerprint_changes() {
        let kv = StubKvOps::new();
        let ctx1 = make_test_context();
        save_consent(&kv, "user-1", &ctx1, 30);

        let mut ctx2 = make_test_context();
        ctx2.raw_tc_string = Some("DIFFERENT".to_owned());
        save_consent(&kv, "user-1", &ctx2, 30);

        let loaded = load_consent(&kv, "user-1").expect("should load updated entry");
        assert_eq!(
            loaded.raw_tc_string,
            Some("DIFFERENT".to_owned()),
            "should reflect updated TC string"
        );
    }

    #[test]
    fn save_consent_skips_empty_consent() {
        let kv = StubKvOps::new();
        let ctx = ConsentContext::default();
        save_consent(&kv, "user-1", &ctx, 30);
        assert!(
            kv.stored.lock().expect("should lock").is_empty(),
            "should not write empty consent"
        );
    }
}
