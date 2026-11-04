//! Production plan dispatch, response admission and mediation observations.
//!
//! Volatility is limited to provider request root `tmax`, each serialized
//! `AuctionResponse.response_time_ms` and `OrchestrationResult.total_time_ms`.
//! Positive tmax values must fit their configured ceiling and are replaced by
//! that ceiling. Elapsed unsigned integers must fit the auction budget and are
//! replaced by zero. Presence/types and every other metadata field are retained.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use edgezero_core::body::Body;
use error_stack::Report;
use http::{Request, Response, Uri};
use serde_json::{Value, json};
use trusted_server_core::auction::orchestrator::DispatchAuctionOutcome;
use trusted_server_core::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, BidStatus,
};
use trusted_server_core::auction::{AuctionOrchestrator, compile_auction_plan};
use trusted_server_core::integrations::adserver_mock::{AdServerMockConfig, AdServerMockProvider};
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec,
    PlatformConfigStore, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
    PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;

use super::{Observation, SOURCE, headers_json, json_bytes};

// The shared Next.js fixture uses a single backend name. This local fixture uses
// real Axum naming so all six provider discriminators survive fan-out correlation.
struct FixturePlatform;

impl PlatformBackend for FixturePlatform {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.naming_policy()
            .predict(spec)
            .map(|backend| backend.name)
            .map_err(|error| error.change_context(PlatformError::Backend))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

// There are no public unavailable config/secret/geo constructors in core. These
// unused capabilities fail at their call site rather than contacting real stores.
#[allow(
    clippy::panic,
    reason = "unexpected fixture service use must fail immediately"
)]
impl PlatformConfigStore for FixturePlatform {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        panic!("baseline auction must not read a config store")
    }
    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        panic!("baseline auction must not write a config store")
    }
    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        panic!("baseline auction must not delete config")
    }
}

#[allow(
    clippy::panic,
    reason = "unexpected fixture service use must fail immediately"
)]
impl PlatformSecretStore for FixturePlatform {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        panic!("baseline auction must not read a secret")
    }
    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        panic!("baseline auction must not write a secret")
    }
    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        panic!("baseline auction must not delete a secret")
    }
}

#[allow(
    clippy::panic,
    reason = "unexpected fixture service use must fail immediately"
)]
impl PlatformGeo for FixturePlatform {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        panic!("baseline auction must not perform a geo lookup")
    }
}

#[derive(Clone, Copy)]
enum MediatorReply {
    HttpError(u16),
    NoBid,
    Winner,
}

#[derive(Default)]
struct Recorded {
    requests: Vec<Value>,
    completion_order: Vec<String>,
}

struct Ready {
    provider: String,
    response: PlatformResponse,
}

struct RecordingClient {
    // Query strings are observed, not discarded, but do not choose the canned reply.
    routes: BTreeMap<(String, String), (String, u32)>,
    reply: MediatorReply,
    recorded: Mutex<Recorded>,
}

fn route_key(uri: &Uri) -> (String, String) {
    (
        uri.authority()
            .expect("should dispatch an absolute upstream URI")
            .as_str()
            .to_owned(),
        uri.path().to_owned(),
    )
}

impl RecordingClient {
    fn prepare(&self, request: PlatformHttpRequest, transport_api: &str) -> Ready {
        let bypass = request.cache_intent.is_bypass();
        let stream_response = request.stream_response;
        let backend = request.backend_name;
        let (parts, body) = request.request.into_parts();
        let (provider, ceiling) = self
            .routes
            .get(&route_key(&parts.uri))
            .expect("should dispatch only the configured provider and mediator routes");
        let bytes = body
            .into_bytes()
            .expect("should send a buffered JSON auction request");
        let mut body_bytes =
            String::from_utf8(bytes.to_vec()).expect("should emit UTF-8 JSON request bytes");
        let mut payload: Value = serde_json::from_str(&body_bytes)
            .expect("should observe actual production JSON request bytes");
        if provider != "adserver_mock" {
            let tmax = payload
                .get_mut("tmax")
                .expect("should retain provider tmax");
            let actual = tmax.as_u64().expect("should retain integer provider tmax");
            assert!(
                actual > 0 && actual <= u64::from(*ceiling),
                "should send a positive bounded logical tmax"
            );
            // Keep field order, whitespace and escaping in the wire observation.
            // This fixture has exactly one compact serialized tmax property.
            let numeric_tmax = format!("\"tmax\":{actual}");
            assert_eq!(
                body_bytes.matches(&numeric_tmax).count(),
                1,
                "should locate only the provider root tmax bytes"
            );
            body_bytes = body_bytes.replacen(&numeric_tmax, &format!("\"tmax\":{ceiling}"), 1);
            *tmax = json!(ceiling);
        }
        self.recorded
            .lock()
            .expect("should record outbound requests")
            .requests
            .push(json!({
                "provider": provider,
                "transport_api": transport_api,
                "backend": backend,
                "method": parts.method.as_str(),
                "uri": parts.uri.to_string(),
                "headers": headers_json(&parts.headers),
                "cache_bypass": bypass,
                "stream_response": stream_response,
                "body": payload,
                "body_bytes": body_bytes,
            }));
        let (status, bytes) = if provider == "adserver_mock" {
            match self.reply {
                MediatorReply::HttpError(status) => (
                    status,
                    b"not JSON: HTTP failure must not be parsed as a winner".to_vec(),
                ),
                MediatorReply::NoBid => (
                    200,
                    serde_json::to_vec(&json!({"seatbid": []}))
                        .expect("should serialize mediator no-bid fixture"),
                ),
                MediatorReply::Winner => (
                    200,
                    serde_json::to_vec(&json!({
                        "seatbid": [{"seat": "a-standard", "bid": [{
                            "id": "mediator-selected-bid", "impid": "baseline-slot", "price": 2.0,
                            "adm": "<div>mediator-selected</div>", "w": 300, "h": 250,
                            "crid": "a-standard-creative", "adomain": ["advertiser.example.com"]
                        }]}]
                    }))
                    .expect("should serialize mediator winner fixture"),
                ),
            }
        } else {
            (200, serde_json::to_vec(&json!({
                "id": "baseline-auction", "cur": "USD",
                "seatbid": [{"seat": provider, "bid": [{
                    "id": format!("{provider}-bid"), "impid": "baseline-slot", "price": 2.0,
                    "adm": format!("<div>{provider}</div>"), "w": 300, "h": 250,
                    "crid": format!("{provider}-creative"), "adomain": ["advertiser.example.com"],
                    "ext": {"creativeurl": format!("https://creative.example.com/{provider}"), "tagtype": "iframe"}
                }]}]
            })).expect("should serialize deterministic OpenRTB provider fixture"))
        };
        Ready {
            provider: provider.clone(),
            response: PlatformResponse::new(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(bytes))
                    .expect("should build a deterministic provider HTTP response"),
            )
            .with_backend_name(backend),
        }
    }

    fn complete(&self, ready: Ready) -> PlatformResponse {
        self.recorded
            .lock()
            .expect("should record actual completion order")
            .completion_order
            .push(ready.provider);
        ready.response
    }
}

#[async_trait(?Send)]
impl PlatformHttpClient for RecordingClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        Ok(self.complete(self.prepare(request, "send")))
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        let ready = self.prepare(request, "send_async");
        let backend = ready
            .response
            .backend_name
            .clone()
            .expect("should correlate every fixture response");
        Ok(PlatformPendingRequest::new(ready).with_backend_name(backend))
    }

    async fn select(
        &self,
        mut pending: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        // Deliberately reverse network completion to distinguish it from the
        // production plan-ordered response and mediator-input sequences.
        let ready = pending
            .pop()
            .expect("should select a launched fixture request")
            .downcast::<Ready>()
            .expect("should recover the recorded fixture response");
        Ok(PlatformSelectResult {
            ready: Ok(self.complete(ready)),
            remaining: pending,
            failed_backend_name: None,
        })
    }

    async fn select_until(
        &self,
        pending: Vec<PlatformPendingRequest>,
        _: Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        // All fixture responses are already complete; this is not a deadline test.
        self.select(pending).await.map(Some)
    }
}

fn normalized_response(response: &AuctionResponse, budget: u32) -> Value {
    let mut value =
        serde_json::to_value(response).expect("should retain the complete provider outcome");
    let elapsed = value
        .get_mut("response_time_ms")
        .expect("should retain response elapsed time");
    assert!(
        elapsed
            .as_u64()
            .is_some_and(|time| time <= u64::from(budget)),
        "should retain a bounded unsigned response time"
    );
    *elapsed = json!(0);
    value
}

fn request(floor: f64) -> AuctionRequest {
    serde_json::from_value(json!({
        "id": "baseline-auction",
        "slots": [{"id": "baseline-slot", "formats": [{"media_type": "banner", "width": 300, "height": 250}],
            "floor_price": floor, "targeting": {"position": "baseline"},
            "bidders": {"example-c": {"placement_id": "example-c"}, "example-f": {"placement_id": "example-f"}}}],
        "publisher": {"domain": "edge.example.com", "page_url": "https://edge.example.com/article"},
        "user": {"id": "baseline-user"},
        "device": {"user_agent": "Baseline Browser", "ip": "192.0.2.10", "geo": null},
        "site": {"domain": "edge.example.com", "page": "https://edge.example.com/article"},
        "context": {"section": "baseline", "page": 1}
    })).expect("should construct the static typed auction request")
}

pub(super) async fn observations() -> Vec<Observation> {
    let settings = Settings::from_toml(SOURCE).expect("should parse baseline auction source");
    let plan = Arc::new(
        compile_auction_plan(&settings).expect("should compile the production baseline plan"),
    );
    assert_eq!(
        plan.providers().len(),
        6,
        "should cover all globally interleaved providers"
    );
    let mediator_config = settings
        .integration_config::<AdServerMockConfig>("adserver_mock")
        .expect("should parse the actual mediator config")
        .expect("should enable the configured mediator");
    let mut routes = BTreeMap::new();
    for provider in plan.providers() {
        let uri: Uri = provider
            .endpoint
            .as_str()
            .parse()
            .expect("should retain validated provider URI");
        routes.insert(
            route_key(&uri),
            (
                provider.id.as_str().to_owned(),
                provider.timeout_ms.min(settings.auction.timeout_ms),
            ),
        );
    }
    let mediator_uri: Uri = mediator_config
        .endpoint
        .parse()
        .expect("should parse mediator URI");
    routes.insert(
        route_key(&mediator_uri),
        ("adserver_mock".to_owned(), mediator_config.timeout_ms),
    );
    let mut rows = Vec::new();
    for (case, reply) in [
        ("http-302", MediatorReply::HttpError(302)),
        ("http-400", MediatorReply::HttpError(400)),
        ("http-429", MediatorReply::HttpError(429)),
        ("http-500", MediatorReply::HttpError(500)),
        ("successful-no-bid", MediatorReply::NoBid),
        ("successful-winner", MediatorReply::Winner),
    ] {
        for (floor, split) in [(1.0, false), (1.0, true), (2.5, false), (2.5, true)] {
            let client = Arc::new(RecordingClient {
                routes: routes.clone(),
                reply,
                recorded: Mutex::new(Recorded::default()),
            });
            let services = RuntimeServices::builder()
                .config_store(Arc::new(FixturePlatform))
                .secret_store(Arc::new(FixturePlatform))
                .kv_store(Arc::new(UnavailableKvStore))
                .backend(Arc::new(FixturePlatform))
                .http_client(Arc::clone(&client) as Arc<dyn PlatformHttpClient>)
                .geo(Arc::new(FixturePlatform))
                .client_info(ClientInfo::default())
                .build();
            let orchestrator = AuctionOrchestrator::from_plan(
                Arc::clone(&plan),
                Some(Arc::new(AdServerMockProvider::new(mediator_config.clone()))),
            );
            let request = request(floor);
            let inbound = Request::builder()
                .method("POST")
                .uri("https://edge.example.com/auction")
                .header("user-agent", "Baseline Browser")
                .header("accept-language", "en-AU")
                .header("dnt", "1")
                .body(Body::empty())
                .expect("should build static downstream transport context");
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: settings.auction.timeout_ms,
                transport_timeout_ms: settings.auction.timeout_ms,
                provider_responses: None,
                services: &services,
            };
            let result = if split {
                let dispatched = match orchestrator.dispatch_auction(&request, &context).await {
                    DispatchAuctionOutcome::Dispatched(dispatched) => Some(dispatched),
                    _ => None,
                }
                .expect("should dispatch all baseline providers before collection");
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("should complete the production auction")
            };
            assert_eq!(
                result.provider_responses.len(),
                6,
                "should retain all provider outcomes"
            );
            assert!(
                result
                    .provider_responses
                    .iter()
                    .all(|response| response.status == BidStatus::Success),
                "should parse one real successful bid from every profile: {:?}",
                result.provider_responses,
            );
            let won = floor <= 2.0 && !matches!(reply, MediatorReply::NoBid);
            assert_eq!(
                result.winning_bids.len(),
                usize::from(won),
                "should retain HTTP-error fallback versus authoritative no-bid and floor semantics"
            );
            if won {
                let bid = &result.winning_bids["baseline-slot"];
                assert_eq!(
                    bid.price,
                    Some(2.0),
                    "should preserve the configured fixture winner price"
                );
                let expected = if matches!(reply, MediatorReply::Winner) {
                    "mediator-selected-bid"
                } else {
                    "a-standard-bid"
                };
                assert_eq!(
                    bid.bid_id.as_deref(),
                    Some(expected),
                    "should retain the selected mediation or local tie winner"
                );
            }
            let recorded = client
                .recorded
                .lock()
                .expect("should inspect captured HTTP exchanges");
            assert_eq!(
                recorded.requests.len(),
                7,
                "should send six providers and one mediator request"
            );
            let mediator_body = &recorded
                .requests
                .last()
                .expect("should dispatch mediator last")["body"];
            let response_order: Vec<&str> = result
                .provider_responses
                .iter()
                .map(|response| response.provider.as_str())
                .collect();
            assert!(
                result.total_time_ms <= u64::from(settings.auction.timeout_ms),
                "should complete the immediate fixture inside its budget"
            );
            rows.push(json!({
                "case": case, "floor": floor, "split_dispatch": split,
                "request": request,
                "plan_order": plan.providers().iter().map(|provider| provider.id.as_str()).collect::<Vec<_>>(),
                "http_requests": recorded.requests,
                "completion_order": recorded.completion_order,
                "response_order": response_order,
                "mediator_input_order": mediator_body["ext"]["bidder_responses"].as_array().expect("should send actual provider responses to mediator").iter().map(|response| response["bidder"].clone()).collect::<Vec<_>>(),
                "provider_responses": result.provider_responses.iter().map(|response| normalized_response(response, settings.auction.timeout_ms)).collect::<Vec<_>>(),
                "mediator_response": result.mediator_response.as_ref().map(|response| normalized_response(response, settings.auction.timeout_ms)),
                "winning_bids": result.winning_bids,
                "total_time_ms": 0,
                "metadata": result.metadata,
            }));
        }
    }
    vec![Observation {
        name: "auction-runtime.json",
        bytes: json_bytes(json!(rows)),
    }]
}
