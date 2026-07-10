//! Prebid EID cookie ingestion.
//!
//! Parses the `ts-eids` cookie written by the TSJS Prebid integration and
//! syncs matched partner UIDs to the KV identity graph.
//!
//! The current cookie format stores a base64-encoded JSON array of full
//! OpenRTB-style `Eid` objects (`{source, uids:[...]}`). For rollout
//! compatibility we also accept the earlier flattened payload shape
//! (`{source, id, atype}` per entry).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use error_stack::Report;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};

use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::MAX_UID_LENGTH;
use super::log_id;
use super::registry::PartnerRegistry;

/// Maximum raw `ts-eids` cookie size accepted before base64 decode.
const MAX_EIDS_COOKIE_BYTES: usize = 8 * 1024;

/// Legacy flattened `ts-eids` cookie entry.
#[derive(Debug, Deserialize)]
struct LegacyCookieEid {
    source: String,
    id: String,
    #[allow(
        dead_code,
        reason = "legacy cookie field is deserialized for compatibility but not emitted"
    )]
    atype: u8,
}

/// OpenRTB-style `ts-eids` cookie entry.
#[derive(Debug, Deserialize)]
struct StructuredCookieEid {
    source: String,
    #[serde(default)]
    uids: Vec<StructuredCookieUid>,
}

#[derive(Debug, Deserialize)]
struct StructuredCookieUid {
    id: String,
    #[serde(default)]
    atype: Option<u8>,
    #[serde(default)]
    ext: Option<JsonValue>,
}

trait PartnerIdBulkWriter {
    fn upsert_partner_ids(
        &self,
        ec_id: &str,
        updates: &[PartnerIdUpdate],
    ) -> Result<(), Report<TrustedServerError>>;
}

impl PartnerIdBulkWriter for KvIdentityGraph {
    fn upsert_partner_ids(
        &self,
        ec_id: &str,
        updates: &[PartnerIdUpdate],
    ) -> Result<(), Report<TrustedServerError>> {
        KvIdentityGraph::upsert_partner_ids(self, ec_id, updates)
    }
}

/// Parses a `ts-eids` cookie value into OpenRTB-style `Eid` entries.
///
/// Accepts both the current structured cookie format and the earlier legacy
/// flattened format for backwards compatibility.
///
/// # Errors
///
/// Returns an error when the cookie exceeds the raw size limit, is not valid
/// base64, or does not contain either supported JSON payload shape.
pub fn parse_prebid_eids_cookie(cookie_value: &str) -> Result<Vec<Eid>, String> {
    if eids_cookie_exceeds_size_limit(cookie_value) {
        return Err(format!(
            "ts-eids cookie too large ({} bytes)",
            cookie_value.len()
        ));
    }

    let bytes = BASE64
        .decode(cookie_value)
        .map_err(|e| format!("base64 decode failed: {e}"))?;

    if let Ok(eids) = serde_json::from_slice::<Vec<LegacyCookieEid>>(&bytes) {
        return Ok(legacy_cookie_eids_to_openrtb(eids));
    }

    let structured = serde_json::from_slice::<Vec<StructuredCookieEid>>(&bytes)
        .map_err(|e| format!("JSON parse failed: {e}"))?;
    Ok(structured_cookie_eids_to_openrtb(structured))
}

/// Parses request-local EID cookies and writes matched partner UIDs to KV.
///
/// `eids_cookie` is the raw base64-encoded `ts-eids` value and
/// `sharedid_cookie` is the raw `sharedId` cookie value. Both values should
/// already be extracted from the request by the caller.
///
/// Best-effort: all errors are logged and swallowed so the main request
/// path is never affected.
pub fn ingest_eid_cookies(
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    ec_id: &str,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
) {
    ingest_eid_cookies_with_writer(eids_cookie, sharedid_cookie, ec_id, kv, registry);
}

/// Collects validated request-local partner updates without performing KV I/O.
pub(crate) fn collect_eid_cookie_updates(
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    if registry.is_empty() {
        return Vec::new();
    }

    let mut updates = Vec::new();
    if let Some(cookie) = eids_cookie {
        updates.extend(collect_prebid_eid_updates(cookie, registry));
    }
    if let Some(cookie) = sharedid_cookie {
        if let Some(update) = collect_sharedid_update(cookie, registry) {
            updates.push(update);
        }
    }
    dedupe_partner_updates(updates)
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
    ingest_eid_cookies(Some(cookie_value), None, ec_id, kv, registry);
}

fn ingest_eid_cookies_with_writer(
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    ec_id: &str,
    writer: &dyn PartnerIdBulkWriter,
    registry: &PartnerRegistry,
) {
    let updates = collect_eid_cookie_updates(eids_cookie, sharedid_cookie, registry);
    if updates.is_empty() {
        return;
    }

    match writer.upsert_partner_ids(ec_id, &updates) {
        Ok(()) => {
            log::debug!(
                "EID cookies: synced {} partner IDs for EC ID '{}'",
                updates.len(),
                log_id(ec_id),
            );
        }
        Err(err) => {
            log::warn!(
                "EID cookies: failed to sync {} partner IDs for EC ID '{}': {err:?}",
                updates.len(),
                log_id(ec_id),
            );
        }
    }
}

fn collect_prebid_eid_updates(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    let Ok(eids) = parse_prebid_eids_cookie(cookie_value) else {
        log::trace!("Prebid EIDs: failed to decode ts-eids cookie; dropping");
        return Vec::new();
    };

    let mut updates = Vec::new();
    for eid in &eids {
        let Some(partner) = registry.find_by_source_domain(&eid.source) else {
            log::debug!("Prebid EIDs: no partner for source '{}'", eid.source);
            continue;
        };

        // KV stores one UID per partner. Preserve the previous cookie-ingestion
        // behavior by syncing the first valid UID under each source, while
        // skipping malformed candidates instead of dropping the whole source.
        let Some(uid) = first_valid_uid(&eid.uids) else {
            log::debug!(
                "Prebid EIDs: no valid uid for source_domain '{}' from source '{}'",
                partner.source_domain,
                eid.source,
            );
            continue;
        };

        updates.push(PartnerIdUpdate::new(&partner.source_domain, &uid.id));
    }

    updates
}

fn dedupe_partner_updates(updates: Vec<PartnerIdUpdate>) -> Vec<PartnerIdUpdate> {
    let mut latest = std::collections::BTreeMap::new();
    for update in updates {
        latest.insert(update.partner_id, update.uid);
    }

    latest
        .into_iter()
        .map(|(partner_id, uid)| PartnerIdUpdate::new(partner_id, uid))
        .collect()
}

fn first_valid_uid(uids: &[Uid]) -> Option<&Uid> {
    uids.iter()
        .filter(|uid| !uid.id.trim().is_empty())
        .find(|uid| !eid_id_exceeds_size_limit(&uid.id))
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
    ingest_eid_cookies(None, Some(cookie_value), ec_id, kv, registry);
}

fn collect_sharedid_update(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Option<PartnerIdUpdate> {
    let cookie_value = cookie_value.trim();
    if cookie_value.is_empty() {
        return None;
    }

    if sharedid_cookie_exceeds_size_limit(cookie_value) {
        log::debug!(
            "SharedID: cookie exceeds MAX_UID_LENGTH ({} bytes)",
            cookie_value.len()
        );
        return None;
    }

    let Some(partner) = registry.find_by_source_domain(SHAREDID_SOURCE_DOMAIN) else {
        log::debug!("SharedID: no partner configured for source '{SHAREDID_SOURCE_DOMAIN}'");
        return None;
    };

    Some(PartnerIdUpdate::new(&partner.source_domain, cookie_value))
}

fn eids_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_EIDS_COOKIE_BYTES
}

fn eid_id_exceeds_size_limit(uid: &str) -> bool {
    uid.len() > MAX_UID_LENGTH
}

fn sharedid_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_UID_LENGTH
}

fn structured_cookie_eids_to_openrtb(entries: Vec<StructuredCookieEid>) -> Vec<Eid> {
    let mut eids = Vec::new();

    for entry in entries {
        if entry.source.is_empty() {
            continue;
        }

        let uids: Vec<_> = entry
            .uids
            .into_iter()
            .filter_map(structured_cookie_uid_to_openrtb)
            .collect();
        if uids.is_empty() {
            continue;
        }

        eids.push(Eid {
            source: entry.source,
            uids,
        });
    }

    eids
}

fn structured_cookie_uid_to_openrtb(uid: StructuredCookieUid) -> Option<Uid> {
    if uid.id.is_empty() {
        return None;
    }

    let ext = match uid.ext {
        Some(JsonValue::Object(_)) => uid.ext,
        _ => None,
    };

    Some(Uid {
        id: uid.id,
        atype: uid.atype,
        ext,
    })
}

fn legacy_cookie_eids_to_openrtb(entries: Vec<LegacyCookieEid>) -> Vec<Eid> {
    entries
        .into_iter()
        .filter(|entry| !entry.source.is_empty() && !entry.id.is_empty())
        .map(|entry| Eid {
            source: entry.source,
            uids: vec![Uid {
                id: entry.id,
                atype: Some(entry.atype),
                ext: None,
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use serde_json::json;

    use crate::ec::registry::PartnerRegistry;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    #[derive(Default)]
    struct RecordingWriter {
        calls: RefCell<Vec<Vec<PartnerIdUpdate>>>,
    }

    impl PartnerIdBulkWriter for RecordingWriter {
        fn upsert_partner_ids(
            &self,
            _ec_id: &str,
            updates: &[PartnerIdUpdate],
        ) -> Result<(), Report<TrustedServerError>> {
            self.calls.borrow_mut().push(updates.to_vec());
            Ok(())
        }
    }

    fn make_test_partner(_id: &str, source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("token-{source_domain}-32-bytes-minimum-value")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    fn make_registry(partners: Vec<(&str, &str)>) -> PartnerRegistry {
        let partners: Vec<_> = partners
            .into_iter()
            .map(|(id, source_domain)| make_test_partner(id, source_domain))
            .collect();
        PartnerRegistry::from_config(&partners).expect("should build partner registry")
    }

    fn encode_json(value: &serde_json::Value) -> String {
        BASE64.encode(serde_json::to_vec(value).expect("should serialize EID JSON"))
    }

    #[test]
    fn parse_prebid_eids_cookie_parses_legacy_flat_payload() {
        let eids = vec![
            json!({"source": "id5-sync.com", "id": "ID5_abc", "atype": 1}),
            json!({"source": "liveramp.com", "id": "LR_xyz", "atype": 3}),
        ];
        let encoded = BASE64.encode(serde_json::to_vec(&eids).expect("should serialize"));

        let decoded = parse_prebid_eids_cookie(&encoded).expect("should decode valid payload");
        assert_eq!(decoded.len(), 2, "should parse both EIDs");
        assert_eq!(decoded[0].source, "id5-sync.com");
        assert_eq!(decoded[0].uids[0].id, "ID5_abc");
        assert_eq!(decoded[1].source, "liveramp.com");
        assert_eq!(decoded[1].uids[0].id, "LR_xyz");
    }

    #[test]
    fn parse_prebid_eids_cookie_parses_structured_payload() {
        let eids = vec![json!({
            "source": "sharedid.org",
            "uids": [
                {"id": "shared_123", "atype": 3},
                {"id": "shared_456", "ext": {"provider": "example"}}
            ]
        })];
        let encoded = BASE64.encode(serde_json::to_vec(&eids).expect("should serialize"));

        let decoded = parse_prebid_eids_cookie(&encoded).expect("should decode valid payload");
        assert_eq!(decoded.len(), 1, "should parse one structured EID entry");
        assert_eq!(decoded[0].source, "sharedid.org");
        assert_eq!(decoded[0].uids.len(), 2, "should preserve multiple UIDs");
        assert_eq!(decoded[0].uids[0].id, "shared_123");
        assert_eq!(decoded[0].uids[0].atype, Some(3));
        assert_eq!(
            decoded[0].uids[1].ext,
            Some(json!({"provider": "example"})),
            "should preserve UID ext objects"
        );
    }

    #[test]
    fn parse_prebid_eids_cookie_rejects_invalid_base64() {
        let result = parse_prebid_eids_cookie("not-valid-base64!!!");
        assert!(result.is_err(), "should reject invalid base64");
    }

    #[test]
    fn parse_prebid_eids_cookie_rejects_invalid_json() {
        let encoded = BASE64.encode(b"not json");
        let result = parse_prebid_eids_cookie(&encoded);
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

    #[test]
    fn prebid_eid_uid_rejects_values_larger_than_uid_limit() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let exact_limit = "x".repeat(MAX_UID_LENGTH);

        assert!(
            eid_id_exceeds_size_limit(&oversized),
            "should reject EID values larger than MAX_UID_LENGTH"
        );
        assert!(
            !eid_id_exceeds_size_limit(&exact_limit),
            "should allow EID values exactly at MAX_UID_LENGTH"
        );
    }

    #[test]
    fn first_valid_uid_skips_oversized_and_uses_later_valid_uid() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let uids = vec![
            Uid {
                id: oversized,
                atype: Some(1),
                ext: None,
            },
            Uid {
                id: "valid-uid".to_owned(),
                atype: Some(1),
                ext: None,
            },
        ];

        let uid = first_valid_uid(&uids).expect("should find later valid UID");
        assert_eq!(uid.id, "valid-uid", "should skip oversized first UID");
    }

    #[test]
    fn first_valid_uid_rejects_whitespace_only_ids() {
        let uids = vec![Uid {
            id: "   ".to_owned(),
            atype: Some(1),
            ext: None,
        }];

        assert!(
            first_valid_uid(&uids).is_none(),
            "should reject whitespace UID"
        );
    }

    #[test]
    fn collect_prebid_eid_updates_collects_multiple_partner_matches() {
        let registry = make_registry(vec![("id5", "id5-sync.com"), ("liveramp", "liveramp.com")]);
        let cookie = encode_json(&json!([
            {"source": "id5-sync.com", "uids": [{"id": "ID5_abc", "atype": 1}]},
            {"source": "liveramp.com", "uids": [{"id": "LR_xyz", "atype": 3}]}
        ]));

        let updates = collect_prebid_eid_updates(&cookie, &registry);

        assert_eq!(updates.len(), 2, "should collect both partner matches");
        assert_eq!(updates[0], PartnerIdUpdate::new("id5-sync.com", "ID5_abc"));
        assert_eq!(updates[1], PartnerIdUpdate::new("liveramp.com", "LR_xyz"));
    }

    #[test]
    fn collect_prebid_eid_updates_skips_unknown_sources() {
        let registry = make_registry(vec![("id5", "id5-sync.com")]);
        let cookie = encode_json(&json!([
            {"source": "unknown.example", "uids": [{"id": "unknown", "atype": 1}]}
        ]));

        let updates = collect_prebid_eid_updates(&cookie, &registry);

        assert!(
            updates.is_empty(),
            "should skip EIDs without configured source-domain partners"
        );
    }

    #[test]
    fn collect_prebid_eid_updates_uses_later_valid_uid_candidate() {
        let registry = make_registry(vec![("id5", "id5-sync.com")]);
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let cookie = encode_json(&json!([
            {
                "source": "id5-sync.com",
                "uids": [
                    {"id": oversized, "atype": 1},
                    {"id": "ID5_valid", "atype": 1}
                ]
            }
        ]));

        let updates = collect_prebid_eid_updates(&cookie, &registry);

        assert_eq!(
            updates,
            vec![PartnerIdUpdate::new("id5-sync.com", "ID5_valid")]
        );
    }

    #[test]
    fn collect_sharedid_update_maps_configured_sharedid_partner() {
        let registry = make_registry(vec![("sharedid", "sharedid.org")]);

        let update = collect_sharedid_update(" shared-cookie-id ", &registry)
            .expect("should collect sharedId update");

        assert_eq!(
            update,
            PartnerIdUpdate::new("sharedid.org", "shared-cookie-id")
        );
    }

    #[test]
    fn dedupe_partner_updates_uses_last_partner_value() {
        let updates = vec![
            PartnerIdUpdate::new("sharedid.org", "prebid-shared"),
            PartnerIdUpdate::new("id5-sync.com", "id5-uid"),
            PartnerIdUpdate::new("sharedid.org", "cookie-shared"),
        ];

        let deduped = dedupe_partner_updates(updates);

        assert_eq!(deduped.len(), 2, "should keep one update per partner");
        assert_eq!(
            deduped,
            vec![
                PartnerIdUpdate::new("id5-sync.com", "id5-uid"),
                PartnerIdUpdate::new("sharedid.org", "cookie-shared"),
            ],
            "should keep the last value for duplicate partners"
        );
    }

    #[test]
    fn ingest_eid_cookies_calls_writer_once_for_multiple_updates() {
        let registry = make_registry(vec![
            ("id5", "id5-sync.com"),
            ("liveramp", "liveramp.com"),
            ("sharedid", "sharedid.org"),
        ]);
        let cookie = encode_json(&json!([
            {"source": "id5-sync.com", "uids": [{"id": "ID5_abc", "atype": 1}]},
            {"source": "liveramp.com", "uids": [{"id": "LR_xyz", "atype": 3}]}
        ]));
        let writer = RecordingWriter::default();

        ingest_eid_cookies_with_writer(
            Some(&cookie),
            Some("shared-cookie-id"),
            "ec-id",
            &writer,
            &registry,
        );

        let calls = writer.calls.borrow();
        assert_eq!(calls.len(), 1, "should perform one bulk writer call");
        assert_eq!(calls[0].len(), 3, "should write all updates in one batch");
        assert_eq!(calls[0][0], PartnerIdUpdate::new("id5-sync.com", "ID5_abc"));
        assert_eq!(calls[0][1], PartnerIdUpdate::new("liveramp.com", "LR_xyz"));
        assert_eq!(
            calls[0][2],
            PartnerIdUpdate::new("sharedid.org", "shared-cookie-id")
        );
    }

    #[test]
    fn ingest_eid_cookies_sharedid_cookie_overrides_prebid_sharedid_update() {
        let registry = make_registry(vec![("sharedid", "sharedid.org")]);
        let cookie = encode_json(&json!([
            {"source": "sharedid.org", "uids": [{"id": "prebid-shared", "atype": 3}]}
        ]));
        let writer = RecordingWriter::default();

        ingest_eid_cookies_with_writer(
            Some(&cookie),
            Some("cookie-shared"),
            "ec-id",
            &writer,
            &registry,
        );

        let calls = writer.calls.borrow();
        assert_eq!(calls.len(), 1, "should perform one bulk writer call");
        assert_eq!(
            calls[0],
            vec![PartnerIdUpdate::new("sharedid.org", "cookie-shared")],
            "should apply sharedId cookie after Prebid EIDs for duplicate source domains"
        );
    }

    #[test]
    fn ingest_eid_cookies_skips_writer_when_no_valid_updates() {
        let registry = make_registry(vec![("id5", "id5-sync.com")]);
        let cookie = encode_json(&json!([
            {"source": "unknown.example", "uids": [{"id": "unknown", "atype": 1}]}
        ]));
        let writer = RecordingWriter::default();

        ingest_eid_cookies_with_writer(Some(&cookie), None, "ec-id", &writer, &registry);

        assert!(
            writer.calls.borrow().is_empty(),
            "should not touch KV writer without valid partner updates"
        );
    }
}
