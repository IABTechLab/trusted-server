//! HTTP endpoint handlers for auction requests.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde_json::Value as JsonValue;

use crate::auction::formats::AdRequest;
use crate::consent::gate_eids_by_consent;
use crate::constants::COOKIE_TS_EIDS;
use crate::ec::eids::{resolve_partner_ids, to_eids};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::log_id;
use crate::ec::prebid_eids::parse_prebid_eids_cookie;
use crate::ec::registry::PartnerRegistry;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::formats::{convert_to_openrtb_response, convert_tsjs_to_auction_request};
use super::types::AuctionContext;
use super::AuctionOrchestrator;

/// Handle auction request from /auction endpoint.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in `OpenRTB` format with creative HTML inline in the `adm` field.
///
/// # Errors
///
/// Returns an error if:
/// - The request body cannot be parsed
/// - The auction request conversion fails (e.g., invalid ad units)
/// - The auction execution fails
/// - The response cannot be serialized
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    kv: Option<&KvIdentityGraph>,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse request body
    let body: AdRequest = serde_json::from_slice(&req.take_body_bytes()).change_context(
        TrustedServerError::Auction {
            message: "Failed to parse auction request body".to_string(),
        },
    )?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    // Story 5 middleware contract: auction is a read-only EC route.
    // It must not generate EC IDs; it only consumes pre-routed context.
    // Only forward the EC ID to auction partners when consent allows it.
    // A returning user may still have a ts-ec cookie but have since
    // withdrawn consent — forwarding that revoked ID to bidders would
    // defeat the consent gating.
    let ec_id = if ec_context.ec_allowed() {
        ec_context.ec_value()
    } else {
        // Intentionally omit persistent identity when EC is disallowed.
        // This keeps the no-consent / GPC path conservative rather than
        // introducing a secondary session-scoped identifier surface here.
        None
    };
    let consent_context = ec_context.consent().clone();
    let geo = ec_context.geo_info().cloned();

    // Parse client-provided EIDs from the current request body. When the
    // current request does not include them, fall back to the persisted
    // `ts-eids` cookie so later requests can still forward the browser's
    // full OpenRTB-style EID structure.
    let client_eids = resolve_client_auction_eids(
        body.eids.as_ref(),
        extract_cookie_value(&req, COOKIE_TS_EIDS).as_deref(),
    );

    // Resolve partner EIDs from the KV identity graph when the user has
    // a valid EC and both KV and partner stores are available.
    let eids = resolve_auction_eids(kv, registry, ec_context);

    // Convert tsjs request format to auction request
    let mut auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &req,
        consent_context,
        ec_id,
        geo,
    )?;

    // Merge current-request client EIDs with KV-resolved EIDs, then apply
    // consent gating before attaching them to the auction request.
    // `gate_eids_by_consent` checks TCF Purpose 1 + 4.
    let merged_eids = merge_auction_eids(client_eids, eids);
    let had_eids = merged_eids.as_ref().is_some_and(|v| !v.is_empty());
    auction_request.user.eids =
        gate_eids_by_consent(merged_eids, auction_request.user.consent.as_ref());
    if had_eids && auction_request.user.eids.is_none() {
        log::warn!("Auction EIDs stripped by TCF consent gating");
    }

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &req,
        client_info: &services.client_info,
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    // Run the auction
    let result = orchestrator
        .run_auction(&auction_request, &context, services)
        .await
        .change_context(TrustedServerError::Auction {
            message: "Auction orchestration failed".to_string(),
        })?;

    log::info!(
        "Auction completed: {} providers, {} winning bids, {}ms total",
        result.provider_responses.len(),
        result.winning_bids.len(),
        result.total_time_ms
    );

    // Convert to OpenRTB response format with inline creative HTML
    convert_to_openrtb_response(&result, settings, &auction_request, ec_context.ec_allowed())
}

/// Resolves partner EIDs from the KV identity graph for bidstream decoration.
///
/// Returns `None` when any prerequisite is missing (no KV store, no partner
/// store, no EC, consent denied). On KV or partner-resolution errors, logs a
/// warning and returns empty EIDs so the auction can proceed in degraded mode.
fn resolve_auction_eids(
    kv: Option<&KvIdentityGraph>,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
) -> Option<Vec<Eid>> {
    let kv = kv?;
    let registry = registry?;

    if !ec_context.ec_allowed() {
        return None;
    }

    let ec_id = ec_context.ec_value()?;

    let entry = match kv.get(ec_id) {
        Ok(Some((entry, _generation))) => entry,
        Ok(None) => return Some(Vec::new()),
        Err(err) => {
            log::warn!(
                "Auction KV read failed for EC ID '{}': {err:?}",
                log_id(ec_id)
            );
            return Some(Vec::new());
        }
    };

    let resolved = resolve_partner_ids(registry, &entry);
    Some(to_eids(&resolved))
}

fn extract_cookie_value(req: &Request, name: &str) -> Option<String> {
    let cookie_header = req.get_header_str("cookie")?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=') {
            if key.trim() == name {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

fn resolve_client_auction_eids(
    raw: Option<&JsonValue>,
    cookie_value: Option<&str>,
) -> Option<Vec<Eid>> {
    parse_client_auction_eids(raw).or_else(|| parse_cookie_auction_eids(cookie_value))
}

fn parse_cookie_auction_eids(cookie_value: Option<&str>) -> Option<Vec<Eid>> {
    let cookie_value = cookie_value?;
    match parse_prebid_eids_cookie(cookie_value) {
        Ok(eids) if eids.is_empty() => None,
        Ok(eids) => Some(eids),
        Err(err) => {
            log::debug!("Auction EIDs: failed to parse ts-eids cookie: {err}");
            None
        }
    }
}

fn parse_client_auction_eids(raw: Option<&JsonValue>) -> Option<Vec<Eid>> {
    let Some(JsonValue::Array(entries)) = raw else {
        return None;
    };

    let mut eids = Vec::new();

    for entry in entries {
        let JsonValue::Object(entry) = entry else {
            log::debug!("Auction EIDs: dropping malformed client EID entry");
            continue;
        };

        let Some(source) = entry
            .get("source")
            .and_then(JsonValue::as_str)
            .filter(|source| !source.is_empty())
            .map(str::to_owned)
        else {
            continue;
        };

        let Some(JsonValue::Array(raw_uids)) = entry.get("uids") else {
            continue;
        };

        let uids: Vec<_> = raw_uids
            .iter()
            .filter_map(parse_client_auction_uid)
            .collect();
        if uids.is_empty() {
            continue;
        }

        eids.push(Eid { source, uids });
    }

    if eids.is_empty() {
        None
    } else {
        Some(eids)
    }
}

fn parse_client_auction_uid(raw: &JsonValue) -> Option<Uid> {
    let JsonValue::Object(uid) = raw else {
        return None;
    };

    let id = uid
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|id| !id.is_empty())?
        .to_owned();

    let atype = uid
        .get("atype")
        .and_then(JsonValue::as_u64)
        .and_then(|atype| u8::try_from(atype).ok());

    let ext = match uid.get("ext") {
        Some(JsonValue::Object(_)) => uid.get("ext").cloned(),
        _ => None,
    };

    Some(Uid { id, atype, ext })
}

fn merge_auction_eids(
    client_eids: Option<Vec<Eid>>,
    resolved_eids: Option<Vec<Eid>>,
) -> Option<Vec<Eid>> {
    let mut merged = Vec::new();

    for eid in resolved_eids
        .into_iter()
        .flatten()
        .chain(client_eids.into_iter().flatten())
    {
        if eid.source.is_empty() {
            continue;
        }

        let source_index = match merged
            .iter()
            .position(|existing: &Eid| existing.source == eid.source)
        {
            Some(index) => index,
            None => {
                merged.push(Eid {
                    source: eid.source.clone(),
                    uids: Vec::new(),
                });
                merged.len() - 1
            }
        };

        for uid in eid.uids {
            if uid.id.is_empty() {
                continue;
            }

            if let Some(existing_uid) = merged[source_index]
                .uids
                .iter_mut()
                .find(|existing| existing.id == uid.id)
            {
                if existing_uid.atype.is_none() {
                    existing_uid.atype = uid.atype;
                }
                if existing_uid.ext.is_none() {
                    existing_uid.ext = uid.ext;
                }
            } else {
                merged[source_index].uids.push(uid);
            }
        }
    }

    merged.retain(|eid| !eid.uids.is_empty());

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;
    use crate::openrtb::Uid;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use serde_json::json;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext::new_for_test(
            ec_value.map(str::to_owned),
            ConsentContext {
                jurisdiction,
                ..ConsentContext::default()
            },
        )
    }

    #[test]
    fn resolve_auction_eids_returns_none_without_kv() {
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(None, Some(&registry), &ec_context);
        assert!(result.is_none(), "should return None when KV is missing");
    }

    #[test]
    fn resolve_auction_eids_returns_none_without_registry() {
        let kv = KvIdentityGraph::new("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(Some(&kv), None, &ec_context);
        assert!(
            result.is_none(),
            "should return None when registry is missing"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_consent_denied() {
        let kv = KvIdentityGraph::new("test_store");
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::Unknown, Some(&ec_id));

        let result = resolve_auction_eids(Some(&kv), Some(&registry), &ec_context);
        assert!(
            result.is_none(),
            "should return None when consent is denied"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_no_ec() {
        let kv = KvIdentityGraph::new("test_store");
        let registry = PartnerRegistry::empty();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let result = resolve_auction_eids(Some(&kv), Some(&registry), &ec_context);
        assert!(
            result.is_none(),
            "should return None when no EC value is present"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_empty_on_kv_miss() {
        let kv = KvIdentityGraph::new("nonexistent_store");
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        // KV store doesn't exist, so the get() call will error — should return
        // empty Vec (degraded mode), not None.
        let result = resolve_auction_eids(Some(&kv), Some(&registry), &ec_context);
        let eids = result.expect("should return Some on KV error (degraded mode)");
        assert!(
            eids.is_empty(),
            "should return empty vec on KV error (degraded mode)"
        );
    }

    #[test]
    fn resolve_client_auction_eids_falls_back_to_ts_eids_cookie() {
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [
                    { "id": "shared_cookie", "atype": 3 },
                    { "id": "shared_cookie_2", "ext": { "provider": "example" } }
                ]
            }
        ]);
        let encoded = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let resolved = resolve_client_auction_eids(None, Some(&encoded))
            .expect("should fall back to structured ts-eids cookie");

        assert_eq!(resolved.len(), 1, "should preserve cookie source entry");
        assert_eq!(resolved[0].source, "sharedid.org");
        assert_eq!(
            resolved[0].uids.len(),
            2,
            "should preserve multiple cookie UIDs"
        );
        assert_eq!(resolved[0].uids[0].id, "shared_cookie");
        assert_eq!(
            resolved[0].uids[1].ext,
            Some(json!({ "provider": "example" })),
            "should preserve UID ext from cookie fallback"
        );
    }

    #[test]
    fn resolve_client_auction_eids_prefers_request_body_over_cookie() {
        let raw = json!([
            {
                "source": "id5-sync.com",
                "uids": [{ "id": "body_uid", "atype": 1 }]
            }
        ]);
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [{ "id": "cookie_uid", "atype": 3 }]
            }
        ]);
        let encoded = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let resolved = resolve_client_auction_eids(Some(&raw), Some(&encoded))
            .expect("should prefer request body EIDs");

        assert_eq!(resolved.len(), 1, "should use request body when present");
        assert_eq!(resolved[0].source, "id5-sync.com");
        assert_eq!(resolved[0].uids[0].id, "body_uid");
    }

    #[test]
    fn parse_client_auction_eids_ignores_malformed_entries() {
        let raw = json!([
            {
                "source": "id5-sync.com",
                "uids": [{ "id": "ID5_abc", "atype": 1 }]
            },
            {
                "source": "broken.example",
                "uids": "not-an-array"
            },
            {
                "source": "sharedid.org",
                "uids": [{ "id": "shared_123" }, { "id": "" }]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid EIDs");

        assert_eq!(parsed.len(), 2, "should keep only valid EID entries");
        assert_eq!(parsed[0].source, "id5-sync.com");
        assert_eq!(parsed[0].uids.len(), 1, "should keep valid UID");
        assert_eq!(parsed[1].source, "sharedid.org");
        assert_eq!(parsed[1].uids.len(), 1, "should drop empty UID values");
    }

    #[test]
    fn parse_client_auction_eids_preserves_uid_ext_and_sanitizes_invalid_atype() {
        let raw = json!([
            {
                "source": "adserver.org",
                "uids": [
                    {
                        "id": "uid-with-ext",
                        "atype": 1,
                        "ext": { "provider": "liveintent.com", "rtiPartner": "TDID" }
                    },
                    {
                        "id": "uid-bad-atype",
                        "atype": 999,
                        "ext": { "keep": true }
                    },
                    {
                        "id": "uid-float-atype",
                        "atype": 1.5
                    }
                ]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid EIDs");

        assert_eq!(parsed.len(), 1, "should keep valid source");
        assert_eq!(parsed[0].uids.len(), 3, "should keep valid UIDs");
        assert_eq!(
            parsed[0].uids[0].atype,
            Some(1),
            "should preserve valid atype"
        );
        assert_eq!(
            parsed[0].uids[0].ext,
            Some(json!({ "provider": "liveintent.com", "rtiPartner": "TDID" })),
            "should preserve uid ext"
        );
        assert_eq!(
            parsed[0].uids[1].atype, None,
            "should drop out-of-range atype without dropping uid"
        );
        assert_eq!(
            parsed[0].uids[1].ext,
            Some(json!({ "keep": true })),
            "should preserve ext when atype is invalid"
        );
        assert_eq!(
            parsed[0].uids[2].atype, None,
            "should drop non-integer atype without dropping uid"
        );
    }

    #[test]
    fn merge_auction_eids_deduplicates_client_and_resolved_ids() {
        let client_eids = Some(vec![Eid {
            source: "id5-sync.com".to_string(),
            uids: vec![Uid {
                id: "ID5_abc".to_string(),
                atype: Some(1),
                ext: None,
            }],
        }]);
        let resolved_eids = Some(vec![
            Eid {
                source: "id5-sync.com".to_string(),
                uids: vec![Uid {
                    id: "ID5_abc".to_string(),
                    atype: Some(1),
                    ext: None,
                }],
            },
            Eid {
                source: "liveramp.com".to_string(),
                uids: vec![Uid {
                    id: "LR_xyz".to_string(),
                    atype: Some(3),
                    ext: None,
                }],
            },
        ]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 2, "should retain distinct EID sources");
        assert_eq!(merged[0].source, "id5-sync.com");
        assert_eq!(merged[0].uids.len(), 1, "should deduplicate matching UIDs");
        assert_eq!(merged[1].source, "liveramp.com");
        assert_eq!(merged[1].uids[0].id, "LR_xyz");
    }

    #[test]
    fn merge_auction_eids_preserves_multiple_uids_per_source() {
        let client_eids = Some(vec![Eid {
            source: "sharedid.org".to_string(),
            uids: vec![Uid {
                id: "shared_client".to_string(),
                atype: None,
                ext: None,
            }],
        }]);
        let resolved_eids = Some(vec![Eid {
            source: "sharedid.org".to_string(),
            uids: vec![Uid {
                id: "shared_server".to_string(),
                atype: Some(3),
                ext: None,
            }],
        }]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 1, "should merge same-source entries");
        assert_eq!(merged[0].uids.len(), 2, "should preserve distinct UIDs");
        assert_eq!(merged[0].uids[0].id, "shared_server");
        assert_eq!(merged[0].uids[1].id, "shared_client");
    }

    #[test]
    fn merge_auction_eids_prefers_server_resolved_metadata_on_conflict() {
        let client_eids = Some(vec![Eid {
            source: "adserver.org".to_string(),
            uids: vec![Uid {
                id: "shared_uid".to_string(),
                atype: Some(1),
                ext: Some(json!({ "provider": "client" })),
            }],
        }]);
        let resolved_eids = Some(vec![Eid {
            source: "adserver.org".to_string(),
            uids: vec![Uid {
                id: "shared_uid".to_string(),
                atype: Some(3),
                ext: Some(json!({ "provider": "server" })),
            }],
        }]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 1, "should merge duplicate source");
        assert_eq!(merged[0].uids.len(), 1, "should deduplicate duplicate uid");
        assert_eq!(
            merged[0].uids[0].atype,
            Some(3),
            "should prefer resolved atype"
        );
        assert_eq!(
            merged[0].uids[0].ext,
            Some(json!({ "provider": "server" })),
            "should prefer resolved ext"
        );
    }
}
