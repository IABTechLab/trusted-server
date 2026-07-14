//! HTTP endpoint handlers for auction requests.

use std::collections::HashMap;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response};
use url::Url;

use crate::auction::admission::{
    admission_denial_response, admit_auction_http, deny_invalid_body, deny_payload_too_large,
    finalize_admission, AdmissionDenial,
};
use crate::auction::formats::AdRequest;
use crate::auction::identity::{
    extract_ts_eids_cookie, resolve_auction_identity, AuctionIdentityInput,
};
use crate::auction::orchestrator::OrchestrationResult;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::formats::{
    apply_auction_response_privacy, convert_to_openrtb_response, convert_tsjs_to_auction_request,
};
use super::telemetry::{
    build_auction_events, emit_auction_events_best_effort_lazy, AuctionObservationContext,
    AuctionTerminalOutcome,
};
use super::types::AuctionContext;
use super::AuctionOrchestrator;
use super::AuctionSource;

/// Maximum accepted JSON body size for `/auction`. Picked to comfortably fit
/// the largest realistic Prebid-derived auction request (hundreds of ad units
/// with EID arrays) while preventing an authenticated client from consuming
/// arbitrary WASM linear memory.
const MAX_AUCTION_BODY_SIZE: usize = crate::auction::admission::MAX_AUCTION_BODY_BYTES;

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
    let draft = match admit_auction_http(
        settings,
        AuctionSource::AuctionApi,
        &req,
        ec_context,
        services.client_info(),
    ) {
        Ok(draft) => draft,
        Err(denial) => return auction_denial_response(&denial),
    };

    let (parts, body) = req.into_parts();
    let body_bytes = match body.into_bytes_bounded(MAX_AUCTION_BODY_SIZE).await {
        Ok(body_bytes) => body_bytes,
        Err(_) => return auction_denial_response(&deny_payload_too_large(draft)),
    };
    let body: AdRequest = match serde_json::from_slice(&body_bytes) {
        Ok(body) => body,
        Err(_) => return auction_denial_response(&deny_invalid_body(draft)),
    };

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    let http_req = Request::from_parts(parts, EdgeBody::empty());
    let page_url = auction_page_url(settings);
    let admission = finalize_admission(draft, page_url);

    // Story 5 middleware contract: auction is a read-only EC route.
    // It must not generate EC IDs; it only consumes pre-routed context.
    // Only forward the EC ID to auction partners when consent allows it.
    let ec_id = ec_context
        .ec_value()
        .filter(|_| admission.identity_allowed());
    let consent_context = admission.consent().clone();

    // Server-side auction consent gate. The publisher-navigation and
    // `/__ts/page-bids` paths fail closed for GDPR/unknown jurisdictions that
    // lack effective TCF Purpose 1. `/auction` is the programmatic entry point
    // for the same server-side auction, so it must gate identically: returning
    // a no-bid response here prevents outbound PBS/APS calls and the forwarding
    // of request-derived signals (UA/IP/geo, and cookies under some Prebid
    // consent-forwarding modes) for traffic that must not run an auction.
    if !admission.auction_allowed() {
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
        let observation = AuctionObservationContext::from_auction_request(
            AuctionSource::AuctionApi,
            &auction_request,
            ec_context,
        );
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Skipped {
                    reason: "consent_denied",
                    elapsed_ms: 0,
                },
            )
        })
        .await;

        let empty_result = OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        };
        return private_auction_response(convert_to_openrtb_response(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
        ));
    }

    let ts_eids_cookie = extract_ts_eids_cookie(&http_req);
    let identity = resolve_auction_identity(AuctionIdentityInput {
        admission: &admission,
        request_eids: body.eids.as_ref(),
        ts_eids_cookie: ts_eids_cookie.as_deref(),
        kv,
        registry,
        ec_context,
    });
    let ec_id = identity.ec_id.as_deref();

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

    auction_request.user.eids = identity.eids;

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &http_req,
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    let observation = AuctionObservationContext::from_auction_request(
        AuctionSource::AuctionApi,
        &auction_request,
        ec_context,
    );

    // Run the auction
    let result = match orchestrator.run_auction(&auction_request, &context).await {
        Ok(result) => result,
        Err(err) => {
            let elapsed_ms = observation.elapsed_ms();
            emit_auction_events_best_effort_lazy(services, || {
                build_auction_events(
                    observation,
                    AuctionTerminalOutcome::ExecutionFailed {
                        request: Some(&auction_request),
                        provider_responses: &[],
                        reason: "execution_failed",
                        elapsed_ms,
                    },
                )
            })
            .await;
            return Err(err.change_context(TrustedServerError::Auction {
                message: "Auction orchestration failed".to_string(),
            }));
        }
    };

    emit_auction_events_best_effort_lazy(services, || {
        build_auction_events(
            observation,
            AuctionTerminalOutcome::Completed {
                request: &auction_request,
                result: &result,
            },
        )
    })
    .await;

    log::info!(
        "Auction completed: {} providers, {} winning bids, {}ms total",
        result.provider_responses.len(),
        result.winning_bids.len(),
        result.total_time_ms
    );

    // Convert to OpenRTB response format with inline creative HTML
    private_auction_response(convert_to_openrtb_response(
        &result,
        settings,
        &auction_request,
        ec_context.ec_allowed(),
    ))
}

fn auction_denial_response(
    denial: &AdmissionDenial,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let mut response =
        admission_denial_response(denial).change_context(TrustedServerError::Auction {
            message: "Failed to build auction admission denial response".to_string(),
        })?;
    apply_auction_response_privacy(&mut response);
    Ok(response)
}

fn private_auction_response(
    response: Result<Response<EdgeBody>, Report<TrustedServerError>>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let mut response = response?;
    apply_auction_response_privacy(&mut response);
    Ok(response)
}

fn auction_page_url(settings: &Settings) -> Url {
    Url::parse(&format!("https://{}", settings.publisher.domain))
        .expect("should build page URL from validated publisher domain")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::build_orchestrator;
    use crate::auction::config::AuctionConfig;
    use crate::auction::identity::test_limits::{
        MAX_CLIENT_EID_SOURCES, MAX_CLIENT_UIDS_PER_SOURCE,
    };
    use crate::auction::identity::{
        merge_auction_eids, parse_client_auction_eids, resolve_auction_eids,
        resolve_client_auction_eids,
    };
    use crate::auction::provider::AuctionProvider;
    use crate::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
    use crate::auction::types::{AuctionRequest, AuctionResponse};
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;
    use crate::constants::COOKIE_TS_EIDS;
    use crate::ec::kv_types::MAX_UID_LENGTH;
    use crate::openrtb::{Eid, Uid};
    use crate::platform::test_support::{
        noop_services, NoopBackend, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore,
    };
    use crate::platform::{ClientInfo, PlatformPendingRequest, PlatformResponse};
    use crate::test_support::tests::create_test_settings;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use http::{header, StatusCode};
    use serde_json::json;
    use serde_json::Value as JsonValue;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingTelemetrySink {
        batches: Mutex<Vec<AuctionEventBatch>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionTelemetrySink for RecordingTelemetrySink {
        async fn emit_auction_events(
            &self,
            _services: &RuntimeServices,
            batch: AuctionEventBatch,
        ) -> Result<(), Report<TrustedServerError>> {
            self.batches
                .lock()
                .expect("should lock telemetry batches")
                .push(batch);
            Ok(())
        }
    }

    fn services_with_telemetry(sink: Arc<RecordingTelemetrySink>) -> RuntimeServices {
        let telemetry_sink: Arc<dyn AuctionTelemetrySink> = sink;
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .auction_telemetry_sink(telemetry_sink)
            .client_info(ClientInfo::default())
            .build()
    }

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext::new_for_test(
            ec_value.map(str::to_owned),
            ConsentContext {
                jurisdiction,
                ..ConsentContext::default()
            },
        )
    }

    fn valid_auction_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ]
        }))
        .expect("should serialize auction body")
    }

    fn admitted_auction_request(body: impl Into<EdgeBody>) -> Request<EdgeBody> {
        Request::builder()
            .method("POST")
            .uri("https://test-publisher.example/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://test-publisher.example")
            .header("x-tsjs-auction", "1")
            .header("sec-fetch-site", "same-origin")
            .body(body.into())
            .expect("should build admitted auction request")
    }

    fn assert_auction_response_privacy(response: &Response<EdgeBody>) {
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&http::HeaderValue::from_static("private, no-store")),
            "auction response should be private and non-cacheable"
        );
        assert_eq!(
            response.headers().get(header::PRAGMA),
            Some(&http::HeaderValue::from_static("no-cache")),
            "auction response should include legacy no-cache header"
        );
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

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("panic-backend".to_string())
        }
    }

    #[tokio::test]
    async fn auction_endpoint_consent_gate_returns_no_bid_without_contacting_providers() {
        // GDPR/unknown jurisdiction lacking effective TCF Purpose 1 must not run
        // a server-side auction. The /auction endpoint must short-circuit to a
        // no-bid response before dispatching to any provider — matching the
        // publisher-navigation and /__ts/page-bids paths.
        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["panic_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(PanicOnBidProvider));
        let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
        let services = services_with_telemetry(Arc::clone(&telemetry_sink));
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
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://test-publisher.com")
            .header("x-tsjs-auction", "1")
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
        let body_bytes = response.into_body().into_bytes().unwrap_or_default();
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

        let batches = telemetry_sink
            .batches
            .lock()
            .expect("should lock telemetry batches");
        assert_eq!(batches.len(), 1, "should emit one telemetry batch");
        let rows = batches[0].rows();
        assert_eq!(rows.len(), 1, "skipped auction should emit one summary row");
        assert_eq!(rows[0].event_kind, "summary");
        assert_eq!(rows[0].terminal_status.as_deref(), Some("skipped"));
        assert_eq!(rows[0].terminal_reason.as_deref(), Some("consent_denied"));
        let ndjson = batches[0]
            .to_ndjson(16 * 1024)
            .expect("should serialize telemetry");
        assert!(
            !ndjson.contains(&ec_id),
            "telemetry must not serialize EC identifiers"
        );
    }

    /// Provider that records whether the auction request it received carried
    /// EIDs, then fails its launch so no real transport handle is needed.
    struct EidCapturingProvider {
        had_eids: Arc<std::sync::Mutex<Option<bool>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for EidCapturingProvider {
        fn provider_name(&self) -> &'static str {
            "eid_capturing_provider"
        }

        async fn request_bids(
            &self,
            request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            *self.had_eids.lock().expect("should lock captured eids") =
                Some(request.user.eids.is_some());
            Err(Report::new(TrustedServerError::Auction {
                message: "capture only".to_string(),
            }))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("parse_response must not run when the launch fails");
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("capture-backend".to_string())
        }
    }

    #[tokio::test]
    async fn auction_strips_client_eids_when_ec_identity_denied() {
        // US-state opt-out via GPC: the server-side auction consent gate still
        // allows a non-personalized auction, but EC identity use is denied
        // (`ec_allowed()` is false) and `gate_eids_by_consent` does not strip
        // because no TCF signal is present and GDPR does not apply. Client EIDs
        // supplied in the request body/cookie must NOT be forwarded — the
        // outgoing auction request must have `user.eids == None`.
        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["eid_capturing_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        let had_eids = Arc::new(std::sync::Mutex::new(None));
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            had_eids: Arc::clone(&had_eids),
        }));
        let services = noop_services();

        // US-state jurisdiction with an explicit GPC opt-out: auction allowed,
        // EC identity denied.
        let ec_context = EcContext::new_for_test(
            None,
            ConsentContext {
                jurisdiction: Jurisdiction::UsState("CA".to_owned()),
                gpc: true,
                ..ConsentContext::default()
            },
        );

        // Persistent EIDs supplied in both the request body and the ts-eids cookie.
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [{ "id": "cookie_uid", "atype": 3 }]
            }
        ]);
        let encoded_cookie = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));
        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ],
            "eids": [
                {
                    "source": "id5-sync.com",
                    "uids": [{ "id": "body_uid", "atype": 1 }]
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://test-publisher.com")
            .header("x-tsjs-auction", "1")
            .header("cookie", format!("{COOKIE_TS_EIDS}={encoded_cookie}"))
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        // The capturing provider fails its launch, so the auction errors overall;
        // the assertion is on the EIDs observed by the provider, not the result.
        let _ = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &ec_context,
            &services,
            req,
        )
        .await;

        assert_eq!(
            *had_eids.lock().expect("should lock captured eids"),
            Some(false),
            "outgoing auction request must carry no EIDs when EC identity is denied"
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
        let kv = KvIdentityGraph::failing("test_store");
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
        let kv = KvIdentityGraph::failing("test_store");
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
        let kv = KvIdentityGraph::failing("test_store");
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
        let kv = KvIdentityGraph::failing("nonexistent_store");
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
    async fn handle_auction_requires_tsjs_header_before_dispatch() {
        let settings = create_test_settings();
        let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
        let services = noop_services();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.example/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://test-publisher.example")
            .body(EdgeBody::from(valid_auction_body()))
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
        .expect("should convert admission denial into response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_auction_response_privacy(&response);
    }

    #[tokio::test]
    async fn handle_auction_maps_admission_denials_to_private_responses() {
        let settings = create_test_settings();
        let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
        let services = noop_services();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let cases = [
            (
                "cross-site fetch metadata",
                Request::builder()
                    .method("POST")
                    .uri("https://test-publisher.example/auction")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ORIGIN, "https://test-publisher.example")
                    .header("x-tsjs-auction", "1")
                    .header("sec-fetch-site", "cross-site")
                    .body(EdgeBody::from(valid_auction_body()))
                    .expect("should build request"),
                StatusCode::FORBIDDEN,
            ),
            (
                "mismatched origin",
                Request::builder()
                    .method("POST")
                    .uri("https://test-publisher.example/auction")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ORIGIN, "https://evil.example")
                    .header("x-tsjs-auction", "1")
                    .body(EdgeBody::from(valid_auction_body()))
                    .expect("should build request"),
                StatusCode::FORBIDDEN,
            ),
            (
                "missing content type",
                Request::builder()
                    .method("POST")
                    .uri("https://test-publisher.example/auction")
                    .header(header::ORIGIN, "https://test-publisher.example")
                    .header("x-tsjs-auction", "1")
                    .body(EdgeBody::from(valid_auction_body()))
                    .expect("should build request"),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (
                "advertised body too large",
                Request::builder()
                    .method("POST")
                    .uri("https://test-publisher.example/auction")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ORIGIN, "https://test-publisher.example")
                    .header("x-tsjs-auction", "1")
                    .header(
                        header::CONTENT_LENGTH,
                        (MAX_AUCTION_BODY_SIZE + 1).to_string(),
                    )
                    .body(EdgeBody::from(valid_auction_body()))
                    .expect("should build request"),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ];

        for (name, req, expected_status) in cases {
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
            .expect("should convert admission denial into response");

            assert_eq!(response.status(), expected_status, "{name}");
            assert_auction_response_privacy(&response);
        }
    }

    #[tokio::test]
    async fn handle_auction_returns_private_bad_request_for_malformed_json() {
        let settings = create_test_settings();
        let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
        let services = noop_services();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let req = admitted_auction_request(b"not-json".as_slice());

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
        .expect("should convert malformed body into response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_auction_response_privacy(&response);
    }

    #[tokio::test]
    async fn handle_auction_disabled_returns_private_no_bid_after_admission_without_dispatch() {
        let mut settings = create_test_settings();
        settings.auction.enabled = false;
        let config = AuctionConfig {
            enabled: false,
            providers: vec!["panic_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(PanicOnBidProvider));
        let services = noop_services();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let req = admitted_auction_request(valid_auction_body());

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
        .expect("disabled auction should return no-bid response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_auction_response_privacy(&response);
        let body_bytes = response.into_body().into_bytes().unwrap_or_default();
        let parsed: JsonValue =
            serde_json::from_slice(&body_bytes).expect("response body should be valid JSON");
        assert!(
            parsed
                .get("seatbid")
                .and_then(JsonValue::as_array)
                .is_none_or(Vec::is_empty),
            "disabled auction must return no bids, got: {parsed}"
        );
    }

    #[test]
    fn auction_rejects_oversized_body() {
        futures::executor::block_on(async {
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
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "https://test.com")
                .header("x-tsjs-auction", "1")
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
        });
    }

    #[test]
    fn auction_rejects_malformed_streaming_body_with_private_bad_request() {
        futures::executor::block_on(async {
            use bytes::Bytes;
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
            let stream = futures::stream::iter([Bytes::from_static(br#"{}"#)]);
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://test.com/auction")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "https://test.com")
                .header("x-tsjs-auction", "1")
                .body(EdgeBody::stream(stream))
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
            .expect("should convert malformed streaming body into response");

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "malformed streaming request body should fail as bad request"
            );
            assert_auction_response_privacy(&response);
        });
    }
}
