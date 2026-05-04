use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use serde_json::json;
use std::time::{Duration, Instant};
use trusted_server_core::auction::build_orchestrator;
use trusted_server_core::bid_cache::{AuctionDeadline, BidMap, InMemoryBidCache};
use trusted_server_core::creative_opportunities::CreativeOpportunitiesFile;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};
use trusted_server_core::request_signing::JWKS_CONFIG_STORE_NAME;
use trusted_server_core::settings::Settings;

use super::{handle_ts_bids_request, route_request};

struct StubJwksConfigStore;

impl PlatformConfigStore for StubJwksConfigStore {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        match key {
            "active-kids" => Ok("test-kid-1".to_string()),
            "test-kid-1" => Ok(
                r#"{"kty":"OKP","crv":"Ed25519","x":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","kid":"test-kid-1","alg":"EdDSA"}"#
                    .to_string(),
            ),
            _ => Err(Report::new(PlatformError::ConfigStore)),
        }
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct NoopSecretStore;

impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct NoopBackend;

impl PlatformBackend for NoopBackend {
    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct NoopHttpClient;

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for NoopHttpClient {
    async fn send(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn send_async(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn select(
        &self,
        _pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

fn create_test_settings() -> Settings {
    let settings = Settings::from_toml(
        r#"
            [[handlers]]
            path = "^/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [edge_cookie]
            secret_key = "test-secret-key"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [consent]
            consent_store = "missing-consent-store"

            [integrations.prebid]
            enabled = true
            server_url = "https://test-prebid.com/openrtb2/auction"

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000
        "#,
    )
    .expect("should parse adapter route test settings");

    assert_eq!(
        JWKS_CONFIG_STORE_NAME, "jwks_store",
        "should keep the stub discovery store aligned with the production constant"
    );

    settings
}

fn test_runtime_services(req: &Request) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(StubJwksConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: req.get_client_ip_addr(),
            tls_protocol: req.get_tls_protocol().map(str::to_string),
            tls_cipher: req.get_tls_cipher_openssl_name().map(str::to_string),
        })
        .build()
}

fn response_body(mut response: Response) -> String {
    response.take_body_str()
}

fn test_bid_cache() -> InMemoryBidCache {
    InMemoryBidCache::new(Duration::from_secs(1), 8)
}

fn empty_creative_opportunities() -> CreativeOpportunitiesFile {
    CreativeOpportunitiesFile::default()
}

fn immediate_deadline() -> AuctionDeadline {
    AuctionDeadline::from_parts(Instant::now(), 1_700_000_000_000)
}

fn slot_bid_map() -> BidMap {
    BidMap::from([(
        "atf_sidebar".to_string(),
        json!({
            "hb_pb": "1.20",
            "hb_bidder": "rubicon",
            "hb_adid": "ad-123",
            "burl": "https://bidder.example/bill"
        }),
    )])
}

#[test]
fn ts_bids_missing_rid_returns_bad_request_no_store() {
    let cache = test_bid_cache();
    let response = handle_ts_bids_request(&Request::get("https://test.com/ts-bids"), &cache);

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "should reject missing request ID"
    );
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
}

#[test]
fn ts_bids_empty_rid_returns_bad_request_no_store() {
    let cache = test_bid_cache();
    let response = handle_ts_bids_request(&Request::get("https://test.com/ts-bids?rid="), &cache);

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "should reject empty request ID"
    );
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
}

#[test]
fn ts_bids_unknown_rid_returns_not_found_no_store() {
    let cache = test_bid_cache();
    let response = handle_ts_bids_request(
        &Request::get("https://test.com/ts-bids?rid=missing"),
        &cache,
    );

    assert_eq!(
        response.get_status(),
        StatusCode::NOT_FOUND,
        "should return 404 for unknown request IDs"
    );
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
}

#[test]
fn ts_bids_completed_rid_returns_bid_json_no_store() {
    let cache = test_bid_cache();
    let bids = slot_bid_map();
    cache.put("rid-1", bids.clone()).expect("should store bids");

    let response =
        handle_ts_bids_request(&Request::get("https://test.com/ts-bids?rid=rid-1"), &cache);
    assert_eq!(
        response.get_status(),
        StatusCode::OK,
        "should return completed bids"
    );
    assert_eq!(
        response.get_header_str(header::CONTENT_TYPE),
        Some("application/json; charset=utf-8"),
        "should return JSON"
    );
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
    let body: serde_json::Value =
        serde_json::from_str(&response_body(response)).expect("should parse JSON body");
    assert_eq!(body, json!(bids), "should serialize bid map");
}

#[test]
fn ts_bids_completed_empty_map_returns_empty_json_no_store() {
    let cache = test_bid_cache();
    cache
        .put_empty("rid-empty")
        .expect("should store empty bid map");

    let response = handle_ts_bids_request(
        &Request::get("https://test.com/ts-bids?rid=rid-empty"),
        &cache,
    );

    assert_eq!(response.get_status(), StatusCode::OK, "should return OK");
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
    assert_eq!(
        response_body(response),
        "{}",
        "should return empty JSON object"
    );
}

#[test]
fn ts_bids_pending_until_original_deadline_returns_empty_json() {
    let cache = test_bid_cache();
    cache
        .mark_pending("rid-pending", immediate_deadline())
        .expect("should mark pending");

    let response = handle_ts_bids_request(
        &Request::get("https://test.com/ts-bids?rid=rid-pending"),
        &cache,
    );

    assert_eq!(
        response.get_status(),
        StatusCode::OK,
        "should return OK after pending deadline"
    );
    assert_eq!(
        response_body(response),
        "{}",
        "should return empty JSON object"
    );
}

#[test]
fn configured_missing_consent_store_only_breaks_consent_routes() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let bid_cache = test_bid_cache();

    let discovery_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services(&discovery_req);
    let discovery_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &discovery_services,
        &empty_creative_opportunities(),
        &bid_cache,
        discovery_req,
    ))
    .expect("should route discovery request");
    assert_eq!(
        discovery_resp.get_status(),
        StatusCode::OK,
        "should keep discovery available when the consent store is unavailable"
    );

    let admin_req = Request::post("https://test.com/admin/keys/rotate");
    let admin_services = test_runtime_services(&admin_req);
    let admin_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &admin_services,
        &empty_creative_opportunities(),
        &bid_cache,
        admin_req,
    ))
    .expect("should route admin request");
    assert_eq!(
        admin_resp.get_status(),
        StatusCode::UNAUTHORIZED,
        "should keep admin auth behavior unchanged when the consent store is unavailable"
    );

    let auction_req = Request::post("https://test.com/auction").with_body(r#"{"adUnits":[]}"#);
    let auction_services = test_runtime_services(&auction_req);
    let auction_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &auction_services,
        &empty_creative_opportunities(),
        &bid_cache,
        auction_req,
    ))
    .expect("should return an error response for auction requests");
    assert_eq!(
        auction_resp.get_status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "should fail auction requests when consent persistence is configured but unavailable"
    );

    let publisher_req = Request::get("https://test.com/articles/example");
    let publisher_services = test_runtime_services(&publisher_req);
    let publisher_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &publisher_services,
        &empty_creative_opportunities(),
        &bid_cache,
        publisher_req,
    ))
    .expect("should return an error response for publisher fallback");
    assert_eq!(
        publisher_resp.get_status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "should scope consent store failures to the consent-dependent routes"
    );
}

#[test]
fn ts_bids_route_is_handled_before_publisher_fallback() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let req = Request::get("https://test.com/ts-bids");
    let runtime_services = test_runtime_services(&req);
    let bid_cache = test_bid_cache();

    let response = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        &empty_creative_opportunities(),
        &bid_cache,
        req,
    ))
    .expect("should route ts-bids request");

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "should handle ts-bids before consent-dependent publisher fallback"
    );
    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should prevent browser caching"
    );
}

#[test]
fn ts_bids_route_keeps_no_store_after_response_header_finalization() {
    let mut settings = create_test_settings();
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=300".to_string(),
    );
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let req = Request::get("https://test.com/ts-bids");
    let runtime_services = test_runtime_services(&req);
    let bid_cache = test_bid_cache();

    let response = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        &empty_creative_opportunities(),
        &bid_cache,
        req,
    ))
    .expect("should route ts-bids request");

    assert_eq!(
        response.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "ts-bids no-store must win over configured response headers"
    );
}
