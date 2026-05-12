//! Prebid EID cookie ingestion.
//!
//! Parses the `ts-eids` cookie (base64-encoded JSON array of `{source, id,
//! atype}` objects written by the TSJS Prebid integration) and syncs matched
//! partner UIDs to the KV identity graph.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;

use super::kv::KvIdentityGraph;
use super::kv_types::MAX_UID_LENGTH;
use super::registry::PartnerRegistry;

/// Minimum seconds between KV writes for the same partner on the same EC.
/// Prevents write thrashing when a user hits many pages quickly.
const SYNC_DEBOUNCE_SECS: u64 = 300;

/// Maximum raw `ts-eids` cookie size accepted before base64 decode.
const MAX_EIDS_COOKIE_BYTES: usize = 8 * 1024;

/// A single flattened EID from the `ts-eids` cookie.
#[derive(Debug, Deserialize)]
struct CookieEid {
    source: String,
    id: String,
    #[allow(dead_code)]
    atype: u8,
}

/// Parses a `ts-eids` cookie value and writes matched partner UIDs to KV.
///
/// `cookie_value` is the raw base64-encoded cookie value, already extracted
/// from the request by the caller.
///
/// Best-effort: all errors are logged and swallowed so the main request
/// path is never affected.
pub fn ingest_prebid_eids(
    cookie_value: &str,
    ec_id: &str,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
) {
    if registry.is_empty() {
        return;
    }

    if eids_cookie_exceeds_size_limit(cookie_value) {
        log::debug!(
            "Prebid EIDs: ts-eids cookie too large ({} bytes)",
            cookie_value.len()
        );
        return;
    }

    let eids = match decode_eids(cookie_value) {
        Ok(eids) => eids,
        Err(err) => {
            log::debug!("Prebid EIDs: failed to decode ts-eids cookie: {err}");
            return;
        }
    };

    let now = super::current_timestamp();

    for eid in &eids {
        let Some(partner) = registry.find_by_source_domain(&eid.source) else {
            log::debug!("Prebid EIDs: no partner for source '{}'", eid.source);
            continue;
        };

        if eid.id.is_empty() {
            continue;
        }

        if eid.id.len() > MAX_UID_LENGTH {
            log::debug!(
                "Prebid EIDs: id for source '{}' exceeds MAX_UID_LENGTH ({} bytes)",
                eid.source,
                eid.id.len()
            );
            continue;
        }

        // Debounce: skip if this partner was synced recently.
        if let Ok(Some((entry, _))) = kv.get(ec_id) {
            if let Some(existing) = entry.ids.get(&partner.id) {
                if now.saturating_sub(existing.synced) < SYNC_DEBOUNCE_SECS {
                    log::debug!(
                        "Prebid EIDs: debouncing partner '{}' (synced {}s ago)",
                        partner.id,
                        now.saturating_sub(existing.synced)
                    );
                    continue;
                }
            }
        }

        match kv.upsert_partner_id(ec_id, &partner.id, &eid.id, now) {
            Ok(_) => {
                log::debug!(
                    "Prebid EIDs: synced partner '{}' from source '{}'",
                    partner.id,
                    eid.source,
                );
            }
            Err(err) => {
                log::warn!(
                    "Prebid EIDs: failed to sync partner '{}': {err:?}",
                    partner.id,
                );
            }
        }
    }
}

/// `SharedID` EID source domain used for partner registry lookup.
const SHAREDID_SOURCE_DOMAIN: &str = "sharedid.org";

/// Ingests a raw `sharedId` cookie value into the KV identity graph.
///
/// Prebid's `SharedID` module writes a `sharedId` cookie directly in the
/// browser. This function reads that value and stores it under the
/// configured `SharedID` partner.
///
/// Best-effort: all errors are logged and swallowed.
pub fn ingest_sharedid_cookie(
    cookie_value: &str,
    ec_id: &str,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
) {
    let cookie_value = cookie_value.trim();
    if cookie_value.is_empty() {
        return;
    }

    if sharedid_cookie_exceeds_size_limit(cookie_value) {
        log::debug!(
            "SharedID: cookie exceeds MAX_UID_LENGTH ({} bytes)",
            cookie_value.len()
        );
        return;
    }

    let Some(partner) = registry.find_by_source_domain(SHAREDID_SOURCE_DOMAIN) else {
        log::debug!("SharedID: no partner configured for source '{SHAREDID_SOURCE_DOMAIN}'");
        return;
    };

    let now = super::current_timestamp();

    // Debounce: skip if this partner was synced recently.
    if let Ok(Some((entry, _))) = kv.get(ec_id) {
        if let Some(existing) = entry.ids.get(&partner.id) {
            if now.saturating_sub(existing.synced) < SYNC_DEBOUNCE_SECS {
                log::debug!(
                    "SharedID: debouncing partner '{}' (synced {}s ago)",
                    partner.id,
                    now.saturating_sub(existing.synced)
                );
                return;
            }
        }
    }

    match kv.upsert_partner_id(ec_id, &partner.id, cookie_value, now) {
        Ok(_) => {
            log::debug!("SharedID: synced partner '{}'", partner.id);
        }
        Err(err) => {
            log::warn!("SharedID: failed to sync partner '{}': {err:?}", partner.id);
        }
    }
}

fn eids_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_EIDS_COOKIE_BYTES
}

fn sharedid_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_UID_LENGTH
}

/// Decodes base64 JSON → `Vec<CookieEid>`.
fn decode_eids(encoded: &str) -> Result<Vec<CookieEid>, String> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("JSON parse failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;

    #[test]
    fn decode_eids_parses_valid_payload() {
        let eids = vec![
            serde_json::json!({"source": "id5-sync.com", "id": "ID5_abc", "atype": 1}),
            serde_json::json!({"source": "liveramp.com", "id": "LR_xyz", "atype": 3}),
        ];
        let encoded = BASE64.encode(serde_json::to_vec(&eids).expect("should serialize"));

        let decoded = decode_eids(&encoded).expect("should decode valid payload");
        assert_eq!(decoded.len(), 2, "should parse both EIDs");
        assert_eq!(decoded[0].source, "id5-sync.com");
        assert_eq!(decoded[0].id, "ID5_abc");
        assert_eq!(decoded[1].source, "liveramp.com");
        assert_eq!(decoded[1].id, "LR_xyz");
    }

    #[test]
    fn decode_eids_rejects_invalid_base64() {
        let result = decode_eids("not-valid-base64!!!");
        assert!(result.is_err(), "should reject invalid base64");
    }

    #[test]
    fn decode_eids_rejects_invalid_json() {
        let encoded = BASE64.encode(b"not json");
        let result = decode_eids(&encoded);
        assert!(result.is_err(), "should reject invalid JSON");
    }

    #[test]
    fn ts_eids_cookie_rejects_oversized_payloads() {
        let oversized = "x".repeat(MAX_EIDS_COOKIE_BYTES + 1);
        let exact_limit = "x".repeat(MAX_EIDS_COOKIE_BYTES);

        assert!(
            eids_cookie_exceeds_size_limit(&oversized),
            "should reject cookies larger than the raw size cap"
        );
        assert!(
            !eids_cookie_exceeds_size_limit(&exact_limit),
            "should allow cookies exactly at the raw size cap"
        );
    }

    #[test]
    fn sharedid_cookie_rejects_values_larger_than_uid_limit() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let exact_limit = "x".repeat(MAX_UID_LENGTH);

        assert!(
            sharedid_cookie_exceeds_size_limit(&oversized),
            "should reject sharedId values larger than MAX_UID_LENGTH"
        );
        assert!(
            !sharedid_cookie_exceeds_size_limit(&exact_limit),
            "should allow sharedId values exactly at MAX_UID_LENGTH"
        );
    }
}
