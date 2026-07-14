//! Shared auction identity resolution.
//!
//! This module owns the common EC, `ts-eids` cookie, request-body EID, and KV
//! identity merge logic used by auction entry points before provider dispatch.

use edgezero_core::body::Body as EdgeBody;
use http::{header, Request};
use serde_json::Value as JsonValue;

use crate::auction::admission::AuctionAdmission;
use crate::consent::{gate_eids_by_consent, ConsentContext};
use crate::constants::COOKIE_TS_EIDS;
use crate::ec::eids::{resolve_partner_ids, to_eids};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_types::MAX_UID_LENGTH;
use crate::ec::log_id;
use crate::ec::prebid_eids::parse_prebid_eids_cookie;
use crate::ec::registry::PartnerRegistry;
use crate::ec::EcContext;
use crate::openrtb::{Eid, Uid};

const MAX_CLIENT_EID_SOURCES: usize = 64;
const MAX_CLIENT_UIDS_PER_SOURCE: usize = 32;
const MAX_CLIENT_EID_SOURCE_BYTES: usize = 255;

/// Inputs for resolving auction identity from all supported sources.
#[derive(Clone, Copy)]
pub(crate) struct AuctionIdentityInput<'a> {
    pub admission: &'a AuctionAdmission,
    pub request_eids: Option<&'a JsonValue>,
    pub ts_eids_cookie: Option<&'a str>,
    pub kv: Option<&'a KvIdentityGraph>,
    pub registry: Option<&'a PartnerRegistry>,
    pub ec_context: &'a EcContext,
}

/// Resolved auction identity after precedence, merge, and consent gates.
#[derive(Clone)]
pub(crate) struct AuctionIdentity {
    pub ec_id: Option<String>,
    pub eids: Option<Vec<Eid>>,
}

/// Resolve EC and EIDs for an admitted auction attempt.
pub(crate) fn resolve_auction_identity(input: AuctionIdentityInput<'_>) -> AuctionIdentity {
    let ec_id = input
        .ec_context
        .ec_value()
        .filter(|_| input.admission.identity_allowed())
        .map(str::to_owned);
    let client_eids = if ec_id.is_some() {
        resolve_client_auction_eids(input.request_eids, input.ts_eids_cookie)
    } else {
        None
    };
    let kv_eids = resolve_auction_eids(input.kv, input.registry, input.ec_context);
    let merged_eids = merge_auction_eids(client_eids, kv_eids);
    let eids = gate_eids_by_consent(merged_eids, Some(input.admission.consent()));

    AuctionIdentity { ec_id, eids }
}

/// Resolve EC and EIDs for the initial-navigation path, which runs before an
/// admission record exists.
///
/// Mirrors [`resolve_auction_identity`] but takes the already-decided EC and
/// consent inputs directly instead of reading them from an
/// [`AuctionAdmission`]. Merges the `ts-eids` browser fallback with EC-keyed KV
/// EIDs and applies the consent gate.
pub(crate) fn resolve_navigation_identity(
    ec_id: Option<&str>,
    ts_eids_cookie: Option<&str>,
    kv: Option<&KvIdentityGraph>,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    consent: &ConsentContext,
) -> AuctionIdentity {
    let ec_id = ec_id.map(str::to_owned);
    let client_eids = if ec_id.is_some() {
        resolve_client_auction_eids(None, ts_eids_cookie)
    } else {
        None
    };
    let kv_eids = resolve_auction_eids(kv, registry, ec_context);
    let merged_eids = merge_auction_eids(client_eids, kv_eids);
    let eids = gate_eids_by_consent(merged_eids, Some(consent));

    AuctionIdentity { ec_id, eids }
}

/// Resolve partner EIDs from the KV identity graph for bidstream decoration.
///
/// Returns `None` when any prerequisite is missing (no KV store, no partner
/// store, no EC, consent denied). On KV or partner-resolution errors, logs a
/// warning and returns empty EIDs so the auction can proceed in degraded mode.
pub(crate) fn resolve_auction_eids(
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

pub(crate) fn extract_cookie_value(req: &Request<EdgeBody>, name: &str) -> Option<String> {
    let cookie_header = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?;
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

pub(crate) fn extract_ts_eids_cookie(req: &Request<EdgeBody>) -> Option<String> {
    extract_cookie_value(req, COOKIE_TS_EIDS)
}

pub(crate) fn resolve_client_auction_eids(
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
        Err(_) => {
            log::trace!("Auction EIDs: failed to parse ts-eids cookie; dropping");
            None
        }
    }
}

pub(crate) fn parse_client_auction_eids(raw: Option<&JsonValue>) -> Option<Vec<Eid>> {
    let Some(JsonValue::Array(entries)) = raw else {
        return None;
    };

    let mut eids = Vec::new();

    for entry in entries {
        if eids.len() >= MAX_CLIENT_EID_SOURCES {
            log::debug!(
                "Auction EIDs: reached max client EID source count ({MAX_CLIENT_EID_SOURCES})"
            );
            break;
        }
        let JsonValue::Object(entry) = entry else {
            log::debug!("Auction EIDs: dropping malformed client EID entry");
            continue;
        };

        let Some(source) = entry
            .get("source")
            .and_then(JsonValue::as_str)
            .filter(|source| !source.trim().is_empty())
            .filter(|source| source.len() <= MAX_CLIENT_EID_SOURCE_BYTES)
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
            .take(MAX_CLIENT_UIDS_PER_SOURCE)
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
        .filter(|id| !id.trim().is_empty())
        .filter(|id| id.len() <= MAX_UID_LENGTH)?
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

pub(crate) fn merge_auction_eids(
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
            if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
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
pub(crate) mod test_limits {
    pub(crate) const MAX_CLIENT_EID_SOURCES: usize = super::MAX_CLIENT_EID_SOURCES;
    pub(crate) const MAX_CLIENT_UIDS_PER_SOURCE: usize = super::MAX_CLIENT_UIDS_PER_SOURCE;
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use edgezero_core::body::Body as EdgeBody;
    use http::{header, Method, Request};
    use serde_json::json;
    use url::Url;

    use crate::auction::admission::{admit_auction_http, finalize_admission};
    use crate::auction::AuctionSource;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::ConsentContext;
    use crate::ec::EcContext;
    use crate::platform::ClientInfo;
    use crate::test_support::tests::create_test_settings;

    use super::*;

    fn admitted_auction() -> (crate::auction::admission::AuctionAdmission, EcContext) {
        let settings = create_test_settings();
        let ec_context = EcContext::new_for_test(
            Some("ec-test".to_string()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..ConsentContext::default()
            },
        );
        let client_info = ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
            tls_ja4: None,
            h2_fingerprint: None,
            server_hostname: None,
            server_region: None,
        };
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://publisher.example")
            .header("x-tsjs-auction", "1")
            .body(EdgeBody::empty())
            .expect("should build admitted auction request");
        let draft = admit_auction_http(
            &settings,
            AuctionSource::AuctionApi,
            &request,
            &ec_context,
            &client_info,
        )
        .expect("should admit auction request");
        let admission = finalize_admission(
            draft,
            Url::parse("https://publisher.example/article").expect("should parse page URL"),
        );
        (admission, ec_context)
    }

    #[test]
    fn identity_resolver_prefers_request_body_eids_over_cookie_eids() {
        let (admission, ec_context) = admitted_auction();
        let body_eids = json!([
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
        let encoded_cookie = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let identity = resolve_auction_identity(AuctionIdentityInput {
            admission: &admission,
            request_eids: Some(&body_eids),
            ts_eids_cookie: Some(&encoded_cookie),
            kv: None,
            registry: None,
            ec_context: &ec_context,
        });

        assert_eq!(identity.ec_id.as_deref(), Some("ec-test"));
        let eids = identity.eids.expect("should resolve body EIDs");
        assert_eq!(eids.len(), 1, "should use request body EIDs");
        assert_eq!(eids[0].source, "id5-sync.com");
        assert_eq!(eids[0].uids[0].id, "body_uid");
    }

    #[test]
    fn identity_resolver_uses_ts_eids_cookie_when_body_eids_are_absent() {
        let (admission, ec_context) = admitted_auction();
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [{ "id": "cookie_uid", "atype": 3 }]
            }
        ]);
        let encoded_cookie = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let identity = resolve_auction_identity(AuctionIdentityInput {
            admission: &admission,
            request_eids: None,
            ts_eids_cookie: Some(&encoded_cookie),
            kv: None,
            registry: None,
            ec_context: &ec_context,
        });

        assert_eq!(identity.ec_id.as_deref(), Some("ec-test"));
        let eids = identity.eids.expect("should resolve cookie EIDs");
        assert_eq!(eids.len(), 1, "should use cookie EIDs");
        assert_eq!(eids[0].source, "sharedid.org");
        assert_eq!(eids[0].uids[0].id, "cookie_uid");
    }
}
