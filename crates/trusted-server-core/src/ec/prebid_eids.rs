//! Browser EID normalization for existing-root enrichment.
//!
//! Auction bodies and the browser-written `ts-eids` cookie supply current-request
//! EIDs. Only registered sources produce UID-only storage updates; browser input
//! cannot confer acquisition authority or lifecycle metadata.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::openrtb::{Eid, Uid};

use super::kv::PartnerIdUpdate;
use super::kv_types::MAX_UID_LENGTH;
use super::registry::PartnerRegistry;

/// Maximum raw `ts-eids` cookie size accepted before base64 decode.
const MAX_EIDS_COOKIE_BYTES: usize = 8 * 1024;

#[derive(Deserialize)]
struct LegacyCookieEid {
    source: String,
    id: String,
    atype: i32,
}

#[derive(Deserialize)]
struct StructuredCookieEid {
    source: String,
    #[serde(default)]
    uids: Vec<StructuredCookieUid>,
    #[serde(default)]
    inserter: Option<String>,
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    mm: Option<i32>,
}

#[derive(Deserialize)]
struct StructuredCookieUid {
    id: String,
    #[serde(default)]
    atype: Option<i32>,
    #[serde(default)]
    ext: Option<JsonValue>,
}

enum DecodedCookieEids {
    Legacy(Vec<LegacyCookieEid>),
    Structured(Vec<StructuredCookieEid>),
}

pub(crate) struct DiagnosticEidSource {
    pub(crate) source: String,
    pub(crate) uids: Vec<String>,
}

pub(crate) struct PrebidEidAnalysis {
    pub(crate) eids: Vec<Eid>,
    pub(crate) diagnostic_sources: Vec<DiagnosticEidSource>,
    pub(crate) updates: Vec<PartnerIdUpdate>,
}

/// Parses a browser `ts-eids` cookie into current-request [`Eid`] entries.
///
/// Accepts structured `OpenRTB` entries and legacy flattened entries. This is
/// browser input, not an authenticated server-capture handoff.
///
/// # Errors
///
/// Returns an error for oversized input, invalid base64, or unsupported JSON.
pub fn parse_prebid_eids_cookie(cookie_value: &str) -> Result<Vec<Eid>, String> {
    decode_prebid_eids_cookie(cookie_value).map(DecodedCookieEids::into_openrtb)
}

fn decode_prebid_eids_cookie(cookie_value: &str) -> Result<DecodedCookieEids, String> {
    if cookie_value.len() > MAX_EIDS_COOKIE_BYTES {
        return Err(format!(
            "ts-eids cookie too large ({} bytes)",
            cookie_value.len()
        ));
    }
    let bytes = BASE64
        .decode(cookie_value)
        .map_err(|error| format!("base64 decode failed: {error}"))?;
    if let Ok(entries) = serde_json::from_slice::<Vec<LegacyCookieEid>>(&bytes) {
        return Ok(DecodedCookieEids::Legacy(entries));
    }
    let entries = serde_json::from_slice::<Vec<StructuredCookieEid>>(&bytes)
        .map_err(|error| format!("JSON parse failed: {error}"))?;
    Ok(DecodedCookieEids::Structured(entries))
}

impl DecodedCookieEids {
    fn diagnostic_sources(&self) -> Vec<DiagnosticEidSource> {
        match self {
            Self::Legacy(entries) => entries
                .iter()
                .filter(|entry| !entry.source.is_empty())
                .map(|entry| DiagnosticEidSource {
                    source: entry.source.clone(),
                    uids: vec![entry.id.clone()],
                })
                .collect(),
            Self::Structured(entries) => entries
                .iter()
                .filter(|entry| !entry.source.is_empty())
                .map(|entry| DiagnosticEidSource {
                    source: entry.source.clone(),
                    uids: entry.uids.iter().map(|uid| uid.id.clone()).collect(),
                })
                .collect(),
        }
    }

    fn into_openrtb(self) -> Vec<Eid> {
        match self {
            Self::Legacy(entries) => entries
                .into_iter()
                .filter(|entry| !entry.source.is_empty() && !entry.id.is_empty())
                .map(|entry| Eid {
                    source: entry.source,
                    uids: vec![Uid {
                        id: entry.id,
                        atype: (entry.atype >= 0).then_some(entry.atype),
                        ext: None,
                    }],
                    ..Eid::default()
                })
                .collect(),
            Self::Structured(entries) => entries
                .into_iter()
                .filter_map(|entry| {
                    if entry.source.is_empty() {
                        return None;
                    }
                    let uids = entry
                        .uids
                        .into_iter()
                        .filter_map(|uid| {
                            if uid.id.is_empty() {
                                return None;
                            }
                            Some(Uid {
                                id: uid.id,
                                atype: uid.atype.filter(|atype| *atype >= 0),
                                ext: uid.ext.filter(JsonValue::is_object),
                            })
                        })
                        .collect::<Vec<_>>();
                    (!uids.is_empty()).then_some(Eid {
                        source: entry.source,
                        uids,
                        inserter: entry.inserter,
                        matcher: entry.matcher,
                        mm: entry.mm,
                    })
                })
                .collect(),
        }
    }
}

pub(crate) fn analyze_prebid_eids_cookie(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Result<PrebidEidAnalysis, String> {
    let decoded = decode_prebid_eids_cookie(cookie_value)?;
    let diagnostic_sources = decoded.diagnostic_sources();
    let eids = decoded.into_openrtb();
    let updates = collect_prebid_eid_updates_from_eids(&eids, registry);
    Ok(PrebidEidAnalysis {
        eids,
        diagnostic_sources,
        updates,
    })
}

fn collect_prebid_eid_updates(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    match parse_prebid_eids_cookie(cookie_value) {
        Ok(eids) => collect_prebid_eid_updates_from_eids(&eids, registry),
        Err(_) => {
            log::trace!("Prebid EIDs: failed to decode ts-eids cookie; dropping");
            Vec::new()
        }
    }
}

/// Collects one valid UID per registered source without KV I/O.
pub(crate) fn collect_prebid_eid_updates_from_eids(
    eids: &[Eid],
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    let updates = eids
        .iter()
        .filter_map(|eid| {
            let partner = registry.find_by_source_domain(&eid.source)?;
            let uid = first_valid_uid(&eid.uids)?;
            Some(PartnerIdUpdate::new(&partner.source_domain, &uid.id))
        })
        .collect();
    dedupe_partner_updates(updates)
}

/// Combines cookie, staged body and sharedId updates into one browser mutation.
///
/// Body updates override `ts-eids`; direct sharedId retains last precedence.
pub(crate) fn collect_browser_eid_updates(
    updates: Vec<PartnerIdUpdate>,
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    let mut combined = eids_cookie
        .map(|cookie| collect_prebid_eid_updates(cookie, registry))
        .unwrap_or_default();
    combined.extend(updates);
    if let Some(cookie) = sharedid_cookie
        && let Some(update) = collect_sharedid_update(cookie, registry)
    {
        combined.push(update);
    }
    dedupe_partner_updates(combined)
}

pub(crate) fn dedupe_partner_updates(updates: Vec<PartnerIdUpdate>) -> Vec<PartnerIdUpdate> {
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
    uids.iter().find(|uid| is_valid_eid_uid(&uid.id))
}

pub(crate) fn is_valid_eid_uid(uid: &str) -> bool {
    !uid.trim().is_empty() && uid.len() <= MAX_UID_LENGTH
}

pub(crate) fn collect_sharedid_update(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Option<PartnerIdUpdate> {
    let cookie_value = cookie_value.trim();
    if !is_valid_eid_uid(cookie_value) {
        return None;
    }
    let partner = registry.find_by_source_domain("sharedid.org")?;
    Some(PartnerIdUpdate::new(&partner.source_domain, cookie_value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::EcPartner;
    use serde_json::json;

    fn registry() -> PartnerRegistry {
        let partners = ["id5-sync.com", "sharedid.org"].map(|source| EcPartner {
            name: source.to_owned(),
            source_domain: source.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            identity_owner: None,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        });
        PartnerRegistry::from_config(&partners).expect("should build registry")
    }

    fn encoded(value: &JsonValue) -> String {
        base64::Engine::encode(
            &BASE64,
            serde_json::to_vec(value).expect("should encode fixture"),
        )
    }

    #[test]
    fn cookie_parser_retains_structured_and_legacy_browser_eids() {
        let legacy = parse_prebid_eids_cookie(&encoded(&json!([
            {"source":"ids.example.com", "id":"legacy", "atype":571187}
        ])))
        .expect("should parse legacy cookie");
        assert_eq!(
            legacy[0].uids[0].atype,
            Some(571187),
            "should retain vendor-specific atype"
        );
        let structured = parse_prebid_eids_cookie(&encoded(&json!([
            {"source":"ids.example.com", "inserter":"browser", "matcher":"", "mm":0,
             "uids":[{"id":"opaque-envelope", "atype":1, "ext":{"linkType":1}}]}
        ])))
        .expect("should parse structured cookie");
        assert_eq!(
            structured[0].uids[0].id, "opaque-envelope",
            "should preserve opaque IDs"
        );
        assert_eq!(
            structured[0].uids[0].ext,
            Some(json!({"linkType":1})),
            "should preserve current-request provider metadata"
        );
        assert_eq!(
            structured[0].matcher.as_deref(),
            Some(""),
            "should retain supplied empty matcher"
        );
        assert_eq!(
            structured[0].mm,
            Some(0),
            "should retain supplied zero match method"
        );
    }

    #[test]
    fn cookie_parser_is_bounded_and_sanitizes_uid_metadata() {
        for invalid in [
            "not-base64".to_owned(),
            encoded(&json!({"not":"array"})),
            "x".repeat(MAX_EIDS_COOKIE_BYTES + 1),
        ] {
            assert!(
                parse_prebid_eids_cookie(&invalid).is_err(),
                "should reject malformed or oversized cookie"
            );
        }
        let eids = parse_prebid_eids_cookie(&encoded(&json!([
            {"source":"ids.example.com", "uids":[{"id":"", "atype":1}, {"id":"valid", "atype":-1, "ext":[]}], "writer":"lockr", "revision":"forged"}
        ]))).expect("should parse tolerant browser shape");
        assert_eq!(eids[0].uids.len(), 1, "should omit empty UID");
        assert_eq!(eids[0].uids[0].atype, None, "should omit negative atype");
        assert_eq!(eids[0].uids[0].ext, None, "should omit non-object ext");
    }

    #[test]
    fn cookie_body_and_sharedid_updates_preserve_legacy_precedence() {
        let value = encoded(&json!([
            {"source":"id5-sync.com", "uids":[{"id":"cookie-id5"}]},
            {"source":"sharedid.org", "uids":[{"id":"cookie-shared"}]},
            {"source":"unknown.example.com", "uids":[{"id":"unregistered"}]}
        ]));
        assert_eq!(
            collect_browser_eid_updates(
                vec![
                    PartnerIdUpdate::new("id5-sync.com", "body-id5"),
                    PartnerIdUpdate::new("sharedid.org", "body-shared")
                ],
                Some(&value),
                Some("direct-shared"),
                &registry()
            ),
            vec![
                PartnerIdUpdate::new("id5-sync.com", "body-id5"),
                PartnerIdUpdate::new("sharedid.org", "direct-shared")
            ],
            "body should beat ts-eids, and sharedId should beat both"
        );
        assert_eq!(
            collect_browser_eid_updates(Vec::new(), Some(&value), None, &registry()),
            vec![
                PartnerIdUpdate::new("id5-sync.com", "cookie-id5"),
                PartnerIdUpdate::new("sharedid.org", "cookie-shared")
            ],
            "cookie-only routes should still ingest registered sources"
        );
    }

    #[test]
    fn body_updates_are_registered_uid_only_and_skip_invalid_candidates() {
        let eids: Vec<Eid> = serde_json::from_value(json!([
            {"source":"ID5-SYNC.COM", "inserter":"forged-owner", "matcher":"", "mm":0,
             "uids":[{"id":" ", "atype":1}, {"id":"opaque-envelope", "ext":{"writer":"forged"}}]},
            {"source":"unknown.example", "uids":[{"id":"current-request-only"}]}
        ]))
        .expect("should parse EIDs");
        assert_eq!(
            collect_prebid_eid_updates_from_eids(&eids, &registry()),
            vec![PartnerIdUpdate::new("id5-sync.com", "opaque-envelope")]
        );
    }

    #[test]
    fn sharedid_combines_with_body_and_retains_direct_cookie_precedence() {
        let updates = vec![
            PartnerIdUpdate::new("sharedid.org", "body-shared"),
            PartnerIdUpdate::new("id5-sync.com", "id5"),
        ];
        assert_eq!(
            collect_browser_eid_updates(updates, None, Some(" cookie-shared "), &registry()),
            vec![
                PartnerIdUpdate::new("id5-sync.com", "id5"),
                PartnerIdUpdate::new("sharedid.org", "cookie-shared")
            ]
        );
    }

    #[test]
    fn uid_limits_are_byte_bounded() {
        assert!(is_valid_eid_uid(&"x".repeat(MAX_UID_LENGTH)));
        assert!(!is_valid_eid_uid(&"x".repeat(MAX_UID_LENGTH + 1)));
        assert!(!is_valid_eid_uid(" \t"));
        assert!(collect_sharedid_update(&"x".repeat(MAX_UID_LENGTH + 1), &registry()).is_none());
        assert!(collect_sharedid_update("valid", &PartnerRegistry::empty()).is_none());
    }
}
