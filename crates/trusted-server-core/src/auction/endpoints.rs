//! HTTP endpoint handlers for auction requests.

use std::collections::HashMap;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{header, Request, Response, StatusCode};
use serde_json::Value as JsonValue;

use crate::auction::formats::AdRequest;
use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::{
    build_completed_auction_events, build_observation_context, AuctionSource,
};
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::constants::COOKIE_TS_EIDS;
use crate::ec::eids::{resolve_partner_ids, to_eids};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_types::MAX_UID_LENGTH;
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

const MAX_CLIENT_EID_SOURCES: usize = 64;
const MAX_CLIENT_UIDS_PER_SOURCE: usize = 32;
const MAX_CLIENT_EID_SOURCE_BYTES: usize = 255;

/// Maximum accepted JSON body size for `/auction`. Picked to comfortably fit
/// the largest realistic Prebid-derived auction request (hundreds of ad units
/// with EID arrays) while preventing an authenticated client from consuming
/// arbitrary WASM linear memory.
const MAX_AUCTION_BODY_SIZE: usize = 256 * 1024;

/// Handle auction request from `POST /auction`.
///
/// Accepts a JSON body matching [`AdRequest`][`super::formats::AdRequest`].
/// The minimum valid request is:
///
/// ```json
/// {
///   "adUnits": [{
///     "code": "atf_sidebar_ad",
///     "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
///   }]
/// }
/// ```
///
/// ## Bidder params: inline vs. stored-request
///
/// Each ad unit's `bids` array is **optional**. When absent or empty the PBS
/// integration falls back to a stored-request keyed by the unit's `code`
/// field (`imp.ext.prebid.storedrequest = { id: "<code>" }`). A PBS stored
/// request must therefore exist for every slot code that omits inline params.
///
/// When `bids` is supplied, each entry's `bidder`/`params` pair is forwarded
/// directly as `imp.ext.prebid.bidder.<bidder>`.
///
/// ## Context passthrough (`config`)
///
/// The optional `config` object is filtered through
/// [`auction.allowed_context_keys`][`crate::settings::AuctionConfig::allowed_context_keys`].
/// Only keys listed there reach the auction providers (e.g. `"permutive_segments"`).
/// All other keys are silently dropped. Values must be either strings or arrays of
/// strings.
///
/// ## Response
///
/// Returns an `OpenRTB 2.x` response. Creative HTML is inlined in each bid's
/// `adm` field after sanitisation and first-party URL rewriting. Response
/// headers include `X-TS-EC` (the caller's Edge Cookie ID) and
/// `X-TS-EC-Fresh` (a freshly generated ID for cookie renewal).
///
/// ## Scroll, refresh, and SPA navigation
///
/// This endpoint is intended for **initial page render** and **programmatic
/// callers** (e.g. slim-Prebid, native apps, server-to-server integrations).
/// It is **not** the intended path for scroll or GPT refresh events.
///
/// **SPA navigation** is handled by `GET /__ts/page-bids`: the client-side SPA
/// hook (`installSpaAuctionHook`) intercepts `pushState`/`replaceState`/`popstate`
/// events and calls that endpoint to fetch fresh slots and bids for each new
/// route, then invokes `window.tsjs.adInit()` with the updated data.
///
/// **Scroll and GPT refresh** are owned by slim-Prebid in Phase 1: it runs
/// post-`window.load`, listens for GPT refresh events, and runs client-side
/// auctions independently of this endpoint.
///
/// A slot-template-aware refresh API (`POST /auction/refresh`) is deferred to a
/// future phase and not designed here.
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
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Reject oversized bodies before any allocation. The Content-Length
    // pre-check stops well-behaved clients early; the post-read check defends
    // against clients that lie about (or omit) the header.
    let content_length_exceeded = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_AUCTION_BODY_SIZE);
    if content_length_exceeded {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(EdgeBody::from(format!(
                "Request body exceeds {MAX_AUCTION_BODY_SIZE} byte limit"
            )))
            .change_context(TrustedServerError::Auction {
                message: "Auction request body exceeded maximum size".to_string(),
            });
    }

    let (parts, body) = req.into_parts();
    let body_bytes = body.into_bytes();
    if body_bytes.len() > MAX_AUCTION_BODY_SIZE {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(EdgeBody::from(format!(
                "Request body exceeds {MAX_AUCTION_BODY_SIZE} byte limit"
            )))
            .change_context(TrustedServerError::Auction {
                message: "Auction request body exceeded maximum size".to_string(),
            });
    }
    let body: AdRequest =
        serde_json::from_slice(&body_bytes).change_context(TrustedServerError::BadRequest {
            message: "Failed to parse auction request body".to_string(),
        })?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    let http_req = Request::from_parts(parts, EdgeBody::empty());

    // Story 5 middleware contract: auction is a read-only EC route.
    // It must not generate EC IDs; it only consumes pre-routed context.
    // Only forward the EC ID to auction partners when consent allows it.
    let ec_id = if ec_context.ec_allowed() {
        ec_context.ec_value()
    } else {
        None
    };
    let consent_context = ec_context.consent().clone();

    // Server-side auction consent gate. The publisher-navigation and
    // `/__ts/page-bids` paths fail closed for GDPR/unknown jurisdictions that
    // lack effective TCF Purpose 1. `/auction` is the programmatic entry point
    // for the same server-side auction, so it must gate identically: returning
    // a no-bid response here prevents outbound PBS/APS calls and the forwarding
    // of request-derived signals (UA/IP/geo, and cookies under some Prebid
    // consent-forwarding modes) for traffic that must not run an auction.
    if !consent_allows_server_side_auction(&consent_context) {
        log::info!(
            "/auction: server-side auction consent gate denied; returning no-bid response without contacting providers"
        );
        // Build the request shape locally (no outbound calls, no geo lookup, no
        // EID resolution) so the no-bid OpenRTB response echoes the request id.
        let auction_request = convert_tsjs_to_auction_request(
            &body,
            settings,
            services,
            &http_req,
            consent_context,
            ec_id,
            None,
        )?;
        let empty_result = OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        };
        return convert_to_openrtb_response(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
        );
    }

    // Parse client-provided EIDs from the current request body. When the
    // current request does not include them, fall back to the persisted
    // `ts-eids` cookie so later requests can still forward the browser's
    // full OpenRTB-style EID structure.
    let client_eids = resolve_client_auction_eids(
        body.eids.as_ref(),
        extract_cookie_value(&http_req, COOKIE_TS_EIDS).as_deref(),
    );

    // Resolve partner EIDs from the KV identity graph when the user has
    // a valid EC and both KV and partner stores are available.
    let eids = resolve_auction_eids(kv, registry, ec_context);

    // Look up geo for device info.
    let geo = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });

    // Convert tsjs request format to auction request
    let mut auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &http_req,
        consent_context,
        ec_id,
        geo,
    )?;

    // Merge current-request client EIDs with KV-resolved EIDs, then apply
    // consent gating before attaching them to the auction request.
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
        request: &http_req,
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    // Run the auction
    let result = orchestrator
        .run_auction(&auction_request, &context)
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

    // Emit completed-auction telemetry. The sink write is buffered and
    // non-blocking in production and a no-op by default in tests, so this never
    // affects the response. Device signals are unknown (`2`) until a later plan
    // threads them through.
    let observation = build_observation_context(
        AuctionSource::AuctionApi,
        &auction_request.publisher.domain,
        auction_request.publisher.page_url.as_deref(),
        auction_request
            .device
            .as_ref()
            .and_then(|device| device.geo.as_ref()),
        auction_request.user.consent.as_ref(),
        2,
        2,
    );
    let slot_count = u16::try_from(auction_request.slots.len()).unwrap_or(u16::MAX);
    let telemetry_rows = build_completed_auction_events(&observation, slot_count, &result);
    services.auction_event_sink().emit(&telemetry_rows);

    // Convert to OpenRTB response format with inline creative HTML
    convert_to_openrtb_response(&result, settings, &auction_request, ec_context.ec_allowed())
}

/// Resolves partner EIDs from the KV identity graph for bidstream decoration.
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

fn extract_cookie_value(req: &Request<EdgeBody>, name: &str) -> Option<String> {
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

fn parse_client_auction_eids(raw: Option<&JsonValue>) -> Option<Vec<Eid>> {
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
mod tests {
    use super::*;
    use crate::auction::config::AuctionConfig;
    use crate::auction::provider::AuctionProvider;
    use crate::auction::types::{AuctionRequest, AuctionResponse};
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;
    use crate::openrtb::Uid;
    use crate::platform::test_support::{
        build_services_with_http_client, noop_services, StubHttpClient,
    };
    use crate::platform::{PlatformHttpRequest, PlatformPendingRequest, PlatformResponse};
    use crate::test_support::tests::create_test_settings;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use serde_json::json;
    use std::sync::Arc;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext::new_for_test(
            ec_value.map(str::to_owned),
            ConsentContext {
                jurisdiction,
                ..ConsentContext::default()
            },
        )
    }

    /// Provider that fails the test if it is ever contacted. Used to prove the
    /// `/auction` consent gate short-circuits before any outbound bid request.
    struct PanicOnBidProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for PanicOnBidProvider {
        fn provider_name(&self) -> &'static str {
            "panic_provider"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            panic!("provider must not be contacted when the consent gate denies the auction");
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("provider must not parse a response when the auction is gated off");
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _timeout_ms: u32) -> Option<String> {
            Some("panic-backend".to_string())
        }
    }

    /// Provider that launches through the stub HTTP client and parses a no-bid
    /// success, so `run_auction` returns a completed `OrchestrationResult`. This
    /// is the path that must emit telemetry.
    struct StubLaunchProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for StubLaunchProvider {
        fn provider_name(&self) -> &'static str {
            "stub_provider"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            let req = PlatformHttpRequest::new(
                Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(EdgeBody::empty())
                    .expect("should build stub bid request"),
                "stub-backend",
            );
            context
                .services
                .http_client()
                .send_async(req)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "stub launch failed".to_string(),
                })
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success(
                "stub_provider",
                vec![],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _timeout_ms: u32) -> Option<String> {
            Some("stub-backend".to_string())
        }
    }

    #[tokio::test]
    async fn auction_endpoint_consent_gate_returns_no_bid_without_contacting_providers() {
        // GDPR/unknown jurisdiction lacking effective TCF Purpose 1 must not run
        // a server-side auction. The /auction endpoint must short-circuit to a
        // no-bid response before dispatching to any provider — matching the
        // publisher-navigation and /__ts/page-bids paths.
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["panic_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(PanicOnBidProvider));
        let services = noop_services();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::Unknown, Some(&ec_id));

        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &ec_context,
            &services,
            req,
        )
        .await
        .expect("gated auction should still return a valid response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "gated auction should return a 200 no-bid response"
        );
        let body_bytes = response.into_body().into_bytes();
        let parsed: JsonValue =
            serde_json::from_slice(&body_bytes).expect("response body should be valid JSON");
        let seatbid_empty = match parsed.get("seatbid").and_then(JsonValue::as_array) {
            Some(seatbid) => seatbid.is_empty(),
            None => true,
        };
        assert!(
            seatbid_empty,
            "gated auction must return no bids, got: {parsed}"
        );
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
    fn parse_client_auction_eids_caps_sources_and_uids() {
        let entries: Vec<_> = (0..(MAX_CLIENT_EID_SOURCES + 5))
            .map(|source_index| {
                let uids: Vec<_> = (0..(MAX_CLIENT_UIDS_PER_SOURCE + 5))
                    .map(|uid_index| json!({ "id": format!("uid-{source_index}-{uid_index}") }))
                    .collect();
                json!({
                    "source": format!("source-{source_index}.example.com"),
                    "uids": uids,
                })
            })
            .collect();
        let raw = JsonValue::Array(entries);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse capped EIDs");

        assert_eq!(
            parsed.len(),
            MAX_CLIENT_EID_SOURCES,
            "should cap client EID sources"
        );
        assert!(
            parsed
                .iter()
                .all(|eid| eid.uids.len() == MAX_CLIENT_UIDS_PER_SOURCE),
            "should cap UIDs per source"
        );
    }

    #[test]
    fn parse_client_auction_eids_drops_whitespace_and_oversized_uids() {
        let raw = json!([
            {
                "source": "id5-sync.com",
                "uids": [
                    { "id": "   " },
                    { "id": "x".repeat(MAX_UID_LENGTH + 1) },
                    { "id": "valid" }
                ]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid UID");

        assert_eq!(parsed.len(), 1, "should retain source with valid UID");
        assert_eq!(parsed[0].uids.len(), 1, "should drop invalid UIDs");
        assert_eq!(parsed[0].uids[0].id, "valid", "should keep valid UID");
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

    #[tokio::test]
    async fn auction_endpoint_emits_completed_telemetry() {
        // A non-regulated, ungated auction completes (one provider launches via
        // the stub HTTP client and parses a no-bid success), so it must emit one
        // summary row tagged auction_api to the injected sink.
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["stub_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(StubLaunchProvider));
        let http_client = Arc::new(StubHttpClient::new());
        http_client.push_response(200, b"{}".to_vec());
        let sink = Arc::new(crate::auction::telemetry::InMemorySink::default());
        let services =
            build_services_with_http_client(http_client).with_auction_event_sink(sink.clone());
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &ec_context,
            &services,
            req,
        )
        .await
        .expect("auction should return a valid response");

        assert_eq!(response.status(), StatusCode::OK, "should return 200");
        let rows = sink.rows();
        assert!(
            rows.iter().any(
                |r| r.event_kind == crate::auction::telemetry::EventKind::Summary
                    && r.auction_source == crate::auction::telemetry::AuctionSource::AuctionApi
            ),
            "should emit a summary row tagged auction_api, got {} rows",
            rows.len()
        );
    }

    #[tokio::test]
    async fn auction_rejects_oversized_body() {
        use edgezero_core::body::Body as EdgeBody;
        use http::{Method, Request as HttpRequest, StatusCode};

        use crate::auction::build_orchestrator;
        use crate::consent::ConsentContext;
        use crate::ec::EcContext;
        use crate::platform::test_support::noop_services;
        use crate::test_support::tests::create_test_settings;

        let settings = create_test_settings();
        let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
        let services = noop_services();
        let ec_context = EcContext::new_for_test(None, ConsentContext::default());
        let oversized = vec![b'x'; MAX_AUCTION_BODY_SIZE + 1];
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("https://test.com/auction")
            .body(EdgeBody::from(oversized))
            .expect("should build request");
        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &ec_context,
            &services,
            req,
        )
        .await
        .expect("should return 413 response for oversized body");
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "should return 413 for auction body over limit"
        );
    }
}
