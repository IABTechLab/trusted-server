//! KV Store consent persistence.
//!
//! Stores and retrieves consent data from a KV Store, keyed by EC ID. This
//! provides consent continuity for returning users whose browsers may not
//! have consent cookies on every request.
//!
//! # Storage layout
//!
//! Each entry uses a single JSON body ([`KvConsentEntry`]) containing the raw
//! consent strings, context flags, and a fingerprint for write-on-change
//! detection.
//!
//! # Change detection
//!
//! Writes only occur when consent signals have actually changed.
//! [`consent_fingerprint`] hashes the raw strings into a compact fingerprint
//! stored in the body's `fp` field. On the next request, the existing
//! fingerprint is compared before writing.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::{ConsentContext, ConsentSource};
use crate::ec::log_id;
use crate::platform::PlatformKvStore;

// ---------------------------------------------------------------------------
// KV body (JSON, stored as value)
// ---------------------------------------------------------------------------

/// Consent data stored in the KV Store body.
///
/// Contains the raw consent strings needed to reconstruct a [`ConsentContext`].
/// Decoded data (TCF, GPP, US Privacy) is not stored — it is re-decoded on
/// read to avoid stale decoded state.
///
/// The `fp` field holds the consent fingerprint for write-on-change detection.
/// Entries written before PR5 lack this field; `#[serde(default)]` treats them
/// as having an empty fingerprint, which always triggers a self-healing
/// re-write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvConsentEntry {
    /// Fingerprint of consent signals for write-on-change detection.
    ///
    /// Written by [`save_consent_to_kv`]. Entries written before PR5 lack
    /// this field; `#[serde(default)]` treats them as having an empty
    /// fingerprint, which always triggers a self-healing re-write.
    #[serde(default)]
    pub fp: String,
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
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Builds a [`KvConsentEntry`] from a [`ConsentContext`].
///
/// Captures only the raw strings and contextual flags. Decoded data is
/// intentionally omitted — it will be re-decoded on read. The `fp` field is
/// initialized to an empty string and must be set by the caller before writing.
#[must_use]
pub fn entry_from_context(ctx: &ConsentContext, now_ds: u64) -> KvConsentEntry {
    KvConsentEntry {
        fp: String::new(),
        raw_tc_string: ctx.raw_tc_string.clone(),
        raw_gpp_string: ctx.raw_gpp_string.clone(),
        gpp_section_ids: ctx.gpp_section_ids.clone(),
        raw_us_privacy: ctx.raw_us_privacy.clone(),
        raw_ac_string: ctx.raw_ac_string.clone(),
        gdpr_applies: ctx.gdpr_applies,
        gpc: ctx.gpc,
        jurisdiction: ctx.jurisdiction.to_string(),
        stored_at_ds: now_ds,
    }
}

/// Converts a [`KvConsentEntry`] into [`crate::consent::types::RawConsentSignals`]
/// suitable for re-decoding via [`crate::consent::build_context_from_signals`].
#[must_use]
pub fn signals_from_entry(entry: &KvConsentEntry) -> crate::consent::types::RawConsentSignals {
    crate::consent::types::RawConsentSignals {
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
    let mut ctx = crate::consent::build_context_from_signals(&signals);

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
// KV Store operations
// ---------------------------------------------------------------------------

/// Checks whether the stored consent fingerprint matches the current one.
///
/// Returns `true` when the stored body's `fp` field equals `new_fp`, meaning
/// no write is needed. Returns `false` when the key is absent, the body
/// cannot be deserialized, or the fingerprint differs.
///
/// Entries written before PR5 have an empty `fp` (via `#[serde(default)]`),
/// which never matches a computed fingerprint and triggers a self-healing
/// re-write.
fn fingerprint_unchanged(store: &dyn PlatformKvStore, key: &str, new_fp: &str) -> bool {
    let bytes = match futures::executor::block_on(store.get_bytes(key)) {
        Ok(Some(bytes)) => bytes,
        _ => return false,
    };

    serde_json::from_slice::<KvConsentEntry>(&bytes)
        .map(|entry| entry.fp == new_fp)
        .unwrap_or(false)
}

/// Loads consent data from the KV store for a given EC ID.
///
/// Returns `Some(ConsentContext)` if a valid entry is found, [`None`] if the
/// key does not exist or deserialization fails. Errors are logged but never
/// propagated — KV failures must not break the request pipeline.
///
/// # Arguments
///
/// * `store` — KV store opened by the adapter.
/// * `ec_id` — Edge Cookie ID used as the KV key.
#[must_use]
pub fn load_consent_from_kv(store: &dyn PlatformKvStore, ec_id: &str) -> Option<ConsentContext> {
    let bytes = match futures::executor::block_on(store.get_bytes(ec_id)) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            log::debug!("Consent KV lookup miss for '{ec_id}'");
            return None;
        }
        Err(e) => {
            log::debug!("Consent KV lookup error for '{ec_id}': {e}");
            return None;
        }
    };

    match serde_json::from_slice::<KvConsentEntry>(&bytes) {
        Ok(entry) => {
            log::info!(
                "Loaded consent from KV store for '{}' (stored_at_ds={})",
                log_id(ec_id),
                entry.stored_at_ds
            );
            Some(context_from_entry(&entry))
        }
        Err(e) => {
            log::warn!(
                "Failed to deserialize consent KV entry for '{}': {e}",
                log_id(ec_id)
            );
            None
        }
    }
}

/// Saves consent data to the KV store, writing only when signals have changed.
///
/// Compares the fingerprint of current consent signals against the fingerprint
/// embedded in the stored entry. If they match, the write is skipped.
/// The fingerprint is embedded in the body so no KV metadata is required.
///
/// # Arguments
///
/// * `store` — KV store opened by the adapter.
/// * `ec_id` — Edge Cookie ID used as the KV key.
/// * `ctx` — Current request's consent context.
/// * `max_age_days` — TTL for the entry, matching `max_consent_age_days`.
pub fn save_consent_to_kv(
    store: &dyn PlatformKvStore,
    ec_id: &str,
    ctx: &ConsentContext,
    max_age_days: u32,
) {
    if ctx.is_empty() {
        log::debug!("Skipping consent KV write: consent is empty");
        return;
    }

    let fp = consent_fingerprint(ctx);

    if fingerprint_unchanged(store, ec_id, &fp) {
        log::debug!(
            "Consent unchanged for '{}' (fp={fp}), skipping write",
            log_id(ec_id)
        );
        return;
    }

    let mut entry = entry_from_context(ctx, crate::consent::now_deciseconds());
    entry.fp = fp.clone();

    let body = match serde_json::to_vec(&entry) {
        Ok(body) => Bytes::from(body),
        Err(e) => {
            log::warn!(
                "Failed to serialize consent entry for '{}': {e}",
                log_id(ec_id)
            );
            return;
        }
    };

    let ttl = std::time::Duration::from_secs(u64::from(max_age_days) * 86_400);

    match futures::executor::block_on(store.put_bytes_with_ttl(ec_id, body, ttl)) {
        Ok(()) => {
            log::info!(
                "Saved consent to KV store for '{}' (fp={fp}, ttl={max_age_days}d)",
                log_id(ec_id)
            );
        }
        Err(e) => {
            log::warn!(
                "Failed to write consent to KV store for '{}': {e}",
                log_id(ec_id)
            );
        }
    }
}

/// Deletes a consent entry from the KV store for a given EC ID.
///
/// Used when a user revokes consent — the existing EC cookie is being
/// expired, so the persisted consent data must also be removed.
///
/// Errors are logged but never propagated — KV failures must not
/// break the request pipeline.
pub fn delete_consent_from_kv(store: &dyn PlatformKvStore, ec_id: &str) {
    match futures::executor::block_on(store.delete(ec_id)) {
        Ok(()) => {
            log::info!(
                "Deleted consent KV entry for '{}' (consent revoked)",
                log_id(ec_id)
            );
        }
        Err(e) => {
            log::warn!(
                "Failed to delete consent KV entry for '{}': {e}",
                log_id(ec_id)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};

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
    fn kv_consent_entry_roundtrip_preserves_fp() {
        let ctx = make_test_context();
        let fp = consent_fingerprint(&ctx);
        let mut entry = entry_from_context(&ctx, 1_000_000);
        entry.fp = fp.clone();
        let json = serde_json::to_string(&entry).expect("should serialize");
        let restored: KvConsentEntry = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(
            restored.fp, fp,
            "should preserve fingerprint through roundtrip"
        );
    }

    #[test]
    fn entry_fits_in_2000_bytes() {
        let ctx = make_test_context();
        let mut entry = entry_from_context(&ctx, 1_000_000);
        entry.fp = consent_fingerprint(&ctx);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            json.len() <= 2000,
            "entry JSON must fit in 2000 bytes, was {} bytes",
            json.len()
        );
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
}

#[cfg(test)]
mod new_api_tests {
    use super::*;
    use edgezero_core::key_value_store::NoopKvStore;

    fn noop() -> NoopKvStore {
        NoopKvStore
    }

    #[test]
    fn load_returns_none_when_key_absent() {
        let result = load_consent_from_kv(&noop(), "some-ec-id");
        assert!(result.is_none(), "should return None when key is absent");
    }

    #[test]
    fn save_does_not_panic_with_noop_store() {
        let ctx = make_test_context();
        save_consent_to_kv(&noop(), "some-ec-id", &ctx, 30);
    }

    #[test]
    fn delete_does_not_panic_with_noop_store() {
        delete_consent_from_kv(&noop(), "some-ec-id");
    }

    #[test]
    fn kv_consent_entry_missing_fp_deserialises_as_empty() {
        let json = r#"{"gdpr_applies":true,"gpc":false,"jurisdiction":"GDPR","stored_at_ds":0}"#;
        let entry: KvConsentEntry =
            serde_json::from_str(json).expect("should deserialize legacy entry");
        assert_eq!(
            entry.fp,
            String::new(),
            "should default fp to empty string for legacy entries"
        );
    }
}
