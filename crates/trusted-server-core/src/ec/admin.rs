//! Admin endpoint for inspecting EC identity graph entries.
//!
//! Serves `GET /_ts/admin/ec` (EC ID taken from the request's `ts-ec`
//! cookie) and `GET /_ts/admin/ec/{id}` (explicit EC ID). Returns the raw
//! stored [`KvEntry`] plus a derived view of the EIDs the auction would
//! attach, so operators can debug KV-to-auction propagation without KV
//! console access.
//!
//! Authentication is enforced by the `^/_ts/admin` basic-auth handler
//! configuration; startup validation rejects configs that leave these paths
//! uncovered (see `Settings::ADMIN_ENDPOINTS`). Because the endpoint is
//! auth-gated and operator-facing, responses intentionally include full
//! internal detail (raw consent strings, partner UIDs, parse errors).

use http::{Request, Response, StatusCode, header};
use serde::Serialize;
use serde_json::Value as JsonValue;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};

use crate::constants::COOKIE_TS_EC;
use crate::error::TrustedServerError;
use crate::openrtb::Eid;

use super::eids::{resolve_partner_ids, to_eids};
use super::generation::is_valid_ec_id;
use super::kv::KvIdentityGraph;
use super::kv_backend::EcKvLookup;
use super::kv_types::{KvEntry, KvMetadata};
use super::log_id;
use super::registry::PartnerRegistry;

/// Route prefix shared by the cookie-based and explicit-ID lookup routes.
const ADMIN_EC_PATH: &str = "/_ts/admin/ec";

/// Successful admin EC lookup payload.
#[derive(Debug, Serialize)]
struct AdminEcLookupResponse {
    /// The EC ID that was looked up.
    ec_id: String,
    /// Platform KV store name the entry was read from.
    store: String,
    /// Store generation marker for the entry.
    generation: u64,
    /// `true` when the entry is a consent-withdrawal tombstone
    /// (`consent.ok = false`). Absent when the body failed to parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    tombstone: Option<bool>,
    /// The stored entry, re-serialized verbatim. Absent when the body
    /// failed to deserialize (see `entry_error` / `raw_body`).
    #[serde(skip_serializing_if = "Option::is_none")]
    entry: Option<JsonValue>,
    /// Deserialization or validation failure detail for the entry body.
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_error: Option<String>,
    /// Raw entry body (lossy UTF-8) when it could not be deserialized.
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_body: Option<String>,
    /// The stored KV metadata mirror, when present and parseable.
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<JsonValue>,
    /// Deserialization failure detail for the metadata, including its raw
    /// value.
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_error: Option<String>,
    /// Derived auction view. Present only when the entry deserializes and
    /// validates — the same precondition the auction read path applies, so
    /// its absence means the auction would attach no KV-derived EIDs.
    /// Live requests additionally gate on per-request consent, which is not
    /// reproducible here.
    #[serde(skip_serializing_if = "Option::is_none")]
    auction: Option<AuctionEidsView>,
}

/// What the auction EID decoration would produce for this entry.
#[derive(Debug, Serialize)]
struct AuctionEidsView {
    /// EIDs the auction would attach to `user.eids`, exactly as produced by
    /// the auction resolution path.
    eids: Vec<Eid>,
    /// Stored partner IDs that the auction resolution filters out, with the
    /// reason each was skipped.
    skipped: Vec<SkippedPartnerId>,
}

/// A stored partner ID excluded from auction EIDs.
#[derive(Debug, Serialize)]
struct SkippedPartnerId {
    /// Partner namespace key in the entry's `ids` map.
    source_domain: String,
    /// Why the auction resolution skips it: `empty_uid`, `not_in_registry`,
    /// or `bidstream_disabled`.
    reason: &'static str,
}

/// Handles `GET /_ts/admin/ec` and `GET /_ts/admin/ec/{id}`.
///
/// Resolves the EC ID from the path when present, falling back to the
/// request's `ts-ec` cookie for the bare route. Responds:
///
/// - `200 OK` with an [`AdminEcLookupResponse`] JSON body when the key
///   exists (including corrupt entries, which are reported with
///   `entry_error` and `raw_body` instead of failing closed);
/// - `400 Bad Request` when the resolved ID is not a valid EC ID;
/// - `404 Not Found` when the key does not exist, or the bare route was
///   called without a `ts-ec` cookie;
/// - `501 Not Implemented` when no EC identity graph is configured.
///
/// # Errors
///
/// Returns [`TrustedServerError::KvStore`] when the store open or read
/// fails.
pub fn handle_admin_ec_lookup(
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let Some(kv) = kv else {
        return Ok(json_error(
            StatusCode::NOT_IMPLEMENTED,
            "EC identity graph is not configured on this deployment",
        ));
    };

    let ec_id = match requested_ec_id(req) {
        Ok(ec_id) => ec_id,
        Err(response) => return Ok(*response),
    };

    let Some(lookup) = kv.lookup_raw(&ec_id)? else {
        log::info!("Admin EC lookup: no entry for '{}'", log_id(&ec_id));
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            "EC entry not found (KV reads are eventually consistent; a very \
             recent entry may not be visible yet)",
        ));
    };

    log::info!("Admin EC lookup: returning entry for '{}'", log_id(&ec_id));
    let payload = build_lookup_response(registry, kv.store_name(), ec_id, &lookup);
    let body =
        serde_json::to_string(&payload).change_context(TrustedServerError::Configuration {
            message: "failed to serialize admin EC lookup response".to_owned(),
        })?;
    Ok(json_response(StatusCode::OK, body))
}

/// Resolves the EC ID to look up from the path or the `ts-ec` cookie.
///
/// Returns the (boxed) error response to send directly when no valid ID is
/// available.
fn requested_ec_id(req: &Request<EdgeBody>) -> Result<String, Box<Response<EdgeBody>>> {
    let remainder = req
        .uri()
        .path()
        .strip_prefix(ADMIN_EC_PATH)
        .unwrap_or("")
        .trim_matches('/');

    let ec_id = if remainder.is_empty() {
        match extract_cookie_value(req, COOKIE_TS_EC) {
            Some(cookie_ec_id) => cookie_ec_id,
            None => {
                return Err(Box::new(json_error(
                    StatusCode::NOT_FOUND,
                    "no EC ID in path and no ts-ec cookie on the request — pass \
                     an explicit id: /_ts/admin/ec/{id}",
                )));
            }
        }
    } else {
        remainder.to_owned()
    };

    if !is_valid_ec_id(&ec_id) {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid EC ID format (expected {64hex}.{6alnum})",
        )));
    }

    Ok(ec_id)
}

/// Builds the success payload from a raw KV lookup.
///
/// Parse failures are reported in the payload rather than propagated, so
/// corrupt entries remain inspectable.
fn build_lookup_response(
    registry: &PartnerRegistry,
    store_name: &str,
    ec_id: String,
    lookup: &EcKvLookup,
) -> AdminEcLookupResponse {
    let mut payload = AdminEcLookupResponse {
        ec_id,
        store: store_name.to_owned(),
        generation: lookup.generation,
        tombstone: None,
        entry: None,
        entry_error: None,
        raw_body: None,
        metadata: None,
        metadata_error: None,
        auction: None,
    };

    match serde_json::from_slice::<KvEntry>(&lookup.body) {
        Ok(entry) => {
            payload.tombstone = Some(!entry.consent.ok);
            match entry.validate() {
                Ok(()) => payload.auction = Some(build_auction_view(registry, &entry)),
                Err(message) => {
                    payload.entry_error = Some(format!(
                        "entry failed validation (auction reads fail closed \
                         and attach no EIDs): {message}"
                    ));
                }
            }
            payload.entry = Some(serde_json::to_value(&entry).expect("should serialize KvEntry"));
        }
        Err(error) => {
            payload.entry_error = Some(format!("failed to deserialize entry: {error}"));
            payload.raw_body = Some(String::from_utf8_lossy(&lookup.body).into_owned());
        }
    }

    match &lookup.metadata {
        None => {}
        Some(bytes) => match serde_json::from_slice::<KvMetadata>(bytes) {
            Ok(metadata) => {
                payload.metadata =
                    Some(serde_json::to_value(&metadata).expect("should serialize KvMetadata"));
            }
            Err(error) => {
                payload.metadata_error = Some(format!(
                    "failed to deserialize metadata: {error} (raw: {})",
                    String::from_utf8_lossy(bytes)
                ));
            }
        },
    }

    payload
}

/// Derives the auction EID view for a valid entry, mirroring the filters in
/// [`resolve_partner_ids`] and reporting why each stored ID was skipped.
fn build_auction_view(registry: &PartnerRegistry, entry: &KvEntry) -> AuctionEidsView {
    let resolved = resolve_partner_ids(registry, entry);
    let eids = to_eids(&resolved);

    let mut skipped = Vec::new();
    for (source_domain, partner_uid) in &entry.ids {
        let reason = if partner_uid.uid.is_empty() {
            "empty_uid"
        } else {
            match registry.get(source_domain) {
                None => "not_in_registry",
                Some(partner) if !partner.bidstream_enabled => "bidstream_disabled",
                Some(_) => continue,
            }
        };
        skipped.push(SkippedPartnerId {
            source_domain: source_domain.clone(),
            reason,
        });
    }

    AuctionEidsView { eids, skipped }
}

fn extract_cookie_value(req: &Request<EdgeBody>, name: &str) -> Option<String> {
    let cookie_header = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=')
            && key.trim() == name
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

fn json_error(status: StatusCode, message: &str) -> Response<EdgeBody> {
    let body = serde_json::json!({ "error": message });
    json_response(status, body.to_string())
}

fn json_response(status: StatusCode, body: String) -> Response<EdgeBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.as_ref())
        .header(header::CACHE_CONTROL, "no-store")
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build admin EC lookup response")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{EcKvStore as _, EcKvWrite, EcKvWriteMode};
    use crate::ec::kv_types::KvPartnerId;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    fn test_ec_id() -> String {
        format!("{}.abc123", "a".repeat(64))
    }

    fn make_test_partner(source_domain: &str, bidstream_enabled: bool) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled,
            api_token: Redacted::new(format!("test-token-{source_domain:-<32}")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    fn test_registry() -> PartnerRegistry {
        PartnerRegistry::from_config(&[
            make_test_partner("bidstream.example", true),
            make_test_partner("disabled.example", false),
        ])
        .expect("should build test partner registry")
    }

    fn get_request(path: &str) -> Request<EdgeBody> {
        Request::builder()
            .method("GET")
            .uri(format!("https://edge.example.com{path}"))
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn get_request_with_cookie(path: &str, cookie: &str) -> Request<EdgeBody> {
        Request::builder()
            .method("GET")
            .uri(format!("https://edge.example.com{path}"))
            .header(header::COOKIE, cookie)
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn kv_with_entry(ec_id: &str, entry: &KvEntry) -> KvIdentityGraph {
        let kv = KvIdentityGraph::in_memory("test-store");
        kv.create(ec_id, entry).expect("should seed KV entry");
        kv
    }

    fn kv_with_raw_body(ec_id: &str, body: &str) -> KvIdentityGraph {
        let metadata = serde_json::json!({ "ok": true, "country": "US", "v": 1 }).to_string();
        let store = InMemoryEcKv::new("test-store");
        store
            .insert(
                ec_id,
                EcKvWrite {
                    body,
                    metadata: &metadata,
                    ttl: Duration::from_secs(60),
                    mode: EcKvWriteMode::Add,
                },
            )
            .expect("should seed raw KV body");
        KvIdentityGraph::new(store)
    }

    fn response_json(response: Response<EdgeBody>) -> JsonValue {
        serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
            .expect("should parse response body as JSON")
    }

    fn sample_entry() -> KvEntry {
        let mut entry = KvEntry::minimal("bidstream.example", "uid-live", 1_741_824_000);
        entry.ids.insert(
            "disabled.example".to_owned(),
            KvPartnerId {
                uid: "uid-disabled".to_owned(),
            },
        );
        entry.ids.insert(
            "unknown.example".to_owned(),
            KvPartnerId {
                uid: "uid-unknown".to_owned(),
            },
        );
        entry
    }

    #[test]
    fn returns_entry_with_auction_view() {
        let ec_id = test_ec_id();
        let kv = kv_with_entry(&ec_id, &sample_entry());
        let req = get_request(&format!("/_ts/admin/ec/{ec_id}"));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "should send no-store on admin responses"
        );

        let json = response_json(response);
        assert_eq!(json["ec_id"], ec_id.as_str());
        assert_eq!(json["store"], "test-store");
        assert_eq!(json["tombstone"], false);
        assert_eq!(
            json["entry"]["ids"]["bidstream.example"]["uid"], "uid-live",
            "should echo the stored entry verbatim"
        );

        let eids = json["auction"]["eids"]
            .as_array()
            .expect("should have auction eids");
        assert_eq!(eids.len(), 1, "should resolve only the bidstream partner");
        assert_eq!(eids[0]["source"], "bidstream.example");
        assert_eq!(eids[0]["uids"][0]["id"], "uid-live");

        let skipped = json["auction"]["skipped"]
            .as_array()
            .expect("should have skipped list");
        assert_eq!(skipped.len(), 2, "should report both filtered partners");
        assert!(
            skipped
                .iter()
                .any(|s| s["source_domain"] == "disabled.example"
                    && s["reason"] == "bidstream_disabled"),
            "should report the bidstream-disabled partner"
        );
        assert!(
            skipped.iter().any(
                |s| s["source_domain"] == "unknown.example" && s["reason"] == "not_in_registry"
            ),
            "should report the unregistered partner"
        );
    }

    #[test]
    fn reports_tombstone_entries() {
        let ec_id = test_ec_id();
        let kv = KvIdentityGraph::in_memory("test-store");
        kv.write_withdrawal_tombstone(&ec_id)
            .expect("should write tombstone");
        let req = get_request(&format!("/_ts/admin/ec/{ec_id}"));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response);
        assert_eq!(json["tombstone"], true, "should flag tombstone entries");
        assert!(
            json["auction"]["eids"]
                .as_array()
                .expect("should have auction eids")
                .is_empty(),
            "tombstone should resolve no EIDs"
        );
    }

    #[test]
    fn missing_entry_returns_404() {
        let kv = KvIdentityGraph::in_memory("test-store");
        let req = get_request(&format!("/_ts/admin/ec/{}", test_ec_id()));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn invalid_id_returns_400() {
        let kv = KvIdentityGraph::in_memory("test-store");
        let req = get_request("/_ts/admin/ec/not-a-valid-id");

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn corrupt_entry_returns_parse_error_and_raw_body() {
        let ec_id = test_ec_id();
        let kv = kv_with_raw_body(&ec_id, "not json at all");
        let req = get_request(&format!("/_ts/admin/ec/{ec_id}"));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "corrupt entries should be inspectable, not opaque errors"
        );
        let json = response_json(response);
        assert!(
            json["entry_error"]
                .as_str()
                .expect("should have entry_error")
                .contains("failed to deserialize"),
            "should describe the parse failure"
        );
        assert_eq!(json["raw_body"], "not json at all");
        assert!(json.get("entry").is_none(), "should omit unparsed entry");
        assert!(
            json.get("auction").is_none(),
            "should omit auction view for unparseable entries"
        );
        assert_eq!(
            json["metadata"]["country"], "US",
            "should still parse the stored metadata"
        );
    }

    #[test]
    fn invalid_schema_version_reports_validation_error() {
        let ec_id = test_ec_id();
        let body = serde_json::json!({
            "v": 99,
            "created": 1000,
            "consent": { "ok": true, "updated": 1000 },
            "geo": { "country": "US" }
        })
        .to_string();
        let kv = kv_with_raw_body(&ec_id, &body);
        let req = get_request(&format!("/_ts/admin/ec/{ec_id}"));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response);
        assert!(
            json["entry_error"]
                .as_str()
                .expect("should have entry_error")
                .contains("failed validation"),
            "should describe the validation failure"
        );
        assert_eq!(json["entry"]["v"], 99, "should still show the parsed entry");
        assert!(
            json.get("auction").is_none(),
            "should omit auction view when the auction read would fail closed"
        );
    }

    #[test]
    fn bare_route_uses_ts_ec_cookie() {
        let ec_id = test_ec_id();
        let kv = kv_with_entry(&ec_id, &sample_entry());
        let req = get_request_with_cookie("/_ts/admin/ec", &format!("other=1; ts-ec={ec_id}; x=2"));

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response);
        assert_eq!(
            json["ec_id"],
            ec_id.as_str(),
            "should resolve the EC ID from the ts-ec cookie"
        );
    }

    #[test]
    fn bare_route_without_cookie_returns_404() {
        let kv = KvIdentityGraph::in_memory("test-store");
        let req = get_request("/_ts/admin/ec");

        let response = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req)
            .expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = response_json(response);
        assert!(
            json["error"]
                .as_str()
                .expect("should have error message")
                .contains("ts-ec cookie"),
            "should explain the missing cookie"
        );
    }

    #[test]
    fn missing_identity_graph_returns_501() {
        let req = get_request(&format!("/_ts/admin/ec/{}", test_ec_id()));

        let response =
            handle_admin_ec_lookup(None, &test_registry(), &req).expect("should handle lookup");

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[test]
    fn kv_read_failure_propagates() {
        let kv = KvIdentityGraph::failing("broken-store");
        let req = get_request(&format!("/_ts/admin/ec/{}", test_ec_id()));

        let result = handle_admin_ec_lookup(Some(&kv), &test_registry(), &req);

        assert!(result.is_err(), "should propagate KV read failures");
    }
}
