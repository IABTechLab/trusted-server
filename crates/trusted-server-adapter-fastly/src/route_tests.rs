use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::http::request::PendingRequest;
use fastly::http::{header, StatusCode};
use fastly::Request;
use serde_json::json;
<<<<<<< HEAD
use trusted_server_core::auction::{
    build_orchestrator, AuctionContext, AuctionOrchestrator, AuctionProvider, AuctionRequest,
    AuctionResponse,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
=======
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
>>>>>>> 680579cf (Fail auction when no providers launch)
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};
use trusted_server_core::request_signing::JWKS_CONFIG_STORE_NAME;
use trusted_server_core::settings::Settings;

use super::route_request;

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

struct DisabledRouteProvider;

impl AuctionProvider for DisabledRouteProvider {
    fn provider_name(&self) -> &'static str {
        "disabled-route"
    }

    fn request_bids(
        &self,
        _request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<PendingRequest, Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Auction {
            message: "disabled route provider should not launch requests".to_string(),
        }))
    }

    fn parse_response(
        &self,
        _response: fastly::Response,
        _response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Auction {
            message: "disabled route provider should not parse responses".to_string(),
        }))
    }

    fn timeout_ms(&self) -> u32 {
        2000
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

fn base_route_settings_toml() -> &'static str {
    r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
        "#
}

fn prebid_integration_toml() -> &'static str {
    r#"
            [integrations.prebid]
            enabled = true
            server_url = "https://test-prebid.com/openrtb2/auction"
        "#
}

fn create_test_settings() -> Settings {
    let base = base_route_settings_toml();
    let prebid = prebid_integration_toml();
    let config = format!(
        r#"{base}

{prebid}

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000
        "#,
    );
    let settings = Settings::from_toml(&config).expect("should parse adapter route test settings");

    assert_eq!(
        JWKS_CONFIG_STORE_NAME, "jwks_store",
        "should keep the stub discovery store aligned with the production constant"
    );

    settings
}

fn create_auction_test_settings(providers: &str) -> Settings {
    let base = base_route_settings_toml();
    let prebid = prebid_integration_toml();
    let config = format!(
        r#"{base}

{prebid}

            [auction]
            enabled = true
            providers = {providers}
            timeout_ms = 2000
        "#,
    );

    Settings::from_toml(&config).expect("should parse adapter auction route test settings")
}

fn build_route_stack(settings: &Settings) -> (AuctionOrchestrator, IntegrationRegistry) {
    let orchestrator = build_orchestrator(settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(settings).expect("should create integration registry");

    (orchestrator, integration_registry)
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

fn route_auction(settings: &Settings, body: impl Into<Vec<u8>>) -> fastly::Response {
    let (orchestrator, integration_registry) = build_route_stack(settings);

    route_auction_with_stack(settings, &orchestrator, &integration_registry, body)
}

fn route_auction_with_stack(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    body: impl Into<Vec<u8>>,
) -> fastly::Response {
    let partner_registry =
        PartnerRegistry::from_config(&settings.ec.partners).expect("should build partner registry");
    let req = Request::post("https://test.com/auction")
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_body(body.into());
    let services = test_runtime_services(&req);

    futures::executor::block_on(route_request(
        settings,
        orchestrator,
        integration_registry,
        &partner_registry,
        &services,
        req,
    ))
    .expect("should route auction request")
    .response
    .expect("should buffer auction response in tests")
}

fn valid_banner_ad_unit_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "adUnits": [
            {
                "code": "div-gpt-ad-1",
                "mediaTypes": {
                    "banner": {
                        "sizes": [[300, 250]]
                    }
                }
            }
        ]
    }))
    .expect("should serialize valid auction route test body")
}

#[test]
fn routes_use_request_local_consent() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let partner_registry =
        PartnerRegistry::from_config(&settings.ec.partners).expect("should build partner registry");

    let discovery_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services(&discovery_req);
    let discovery_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &discovery_services,
        discovery_req,
    ))
    .expect("should route discovery request");
    let discovery_response = discovery_resp
        .response
        .expect("should buffer discovery response in tests");
    assert_eq!(
        discovery_response.get_status(),
        StatusCode::OK,
        "should keep discovery available with request-local consent"
    );

    let admin_req = Request::post("https://test.com/_ts/admin/keys/rotate");
    let admin_services = test_runtime_services(&admin_req);
    let admin_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &admin_services,
        admin_req,
    ))
    .expect("should route admin request");
    let admin_response = admin_resp
        .response
        .expect("should buffer admin response in tests");
    assert_eq!(
        admin_response.get_status(),
        StatusCode::UNAUTHORIZED,
        "should keep admin auth behavior unchanged with request-local consent"
    );

    // Routes no longer depend on a separate consent KV store. Live consent is
    // request-local, and EC lifecycle state uses the EC identity store only.
}

#[test]
fn malformed_auction_json_returns_bad_request() {
    let settings = create_auction_test_settings(r#"["prebid"]"#);

    let mut response = route_auction(&settings, "{not-json");

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "should reject malformed JSON as a client request error"
    );
    assert!(
        response.take_body_str().contains("Bad request"),
        "should return a client-facing bad request message"
    );
}

#[test]
fn invalid_auction_banner_size_returns_bad_request() {
    let settings = create_auction_test_settings(r#"["prebid"]"#);
    let body = serde_json::to_vec(&json!({
        "adUnits": [
            {
                "code": "div-gpt-ad-1",
                "mediaTypes": {
                    "banner": {
                        "sizes": [[300]]
                    }
                }
            }
        ]
    }))
    .expect("should serialize invalid auction route test body");

    let response = route_auction(&settings, body);

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "should reject semantically invalid banner sizes as a client request error"
    );
}

#[test]
fn auction_request_with_empty_provider_list_returns_bad_gateway() {
    let settings = create_auction_test_settings("[]");

    let response = route_auction(&settings, valid_banner_ad_unit_body());

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_GATEWAY,
        "should surface no-provider orchestration failures as gateway errors"
    );
}

#[test]
fn auction_request_with_disabled_provider_returns_bad_gateway() {
    let settings = create_auction_test_settings(r#"["disabled-route"]"#);
    let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
    orchestrator.register_provider(Arc::new(DisabledRouteProvider));
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let response = route_auction_with_stack(
        &settings,
        &orchestrator,
        &integration_registry,
        valid_banner_ad_unit_body(),
    );

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_GATEWAY,
        "should map skipped-provider launch failures to gateway errors"
    );
}
