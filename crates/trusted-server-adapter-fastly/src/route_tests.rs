use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::response_builder as edge_response_builder;
use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::Request;
use fastly::http::request::PendingRequest;
use fastly::http::{Method, StatusCode, header};
use serde_json::json;
use trusted_server_core::auction::{
    AuctionContext, AuctionOrchestrator, AuctionProvider, AuctionRequest, AuctionResponse,
    build_orchestrator,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};
use trusted_server_core::request_signing::JWKS_CONFIG_STORE_NAME;
use trusted_server_core::settings::{
    AssetImageOptimizerConfig, AssetOriginAuth, ImageOptimizerProfileSet, ImageOptimizerSettings,
    ProxyAssetRoute, S3SigV4AuthConfig, Settings,
};

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

struct HashMapSecretStore {
    data: HashMap<String, Vec<u8>>,
}

impl HashMapSecretStore {
    fn new(data: HashMap<String, Vec<u8>>) -> Self {
        Self { data }
    }
}

impl PlatformSecretStore for HashMapSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        self.data
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::SecretStore))
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

struct RecordingHttpClient {
    calls: Mutex<Vec<RecordedHttpCall>>,
    response_status: StatusCode,
    response_headers: Vec<(String, String)>,
}

impl RecordingHttpClient {
    fn new(response_status: StatusCode) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response_status,
            response_headers: Vec::new(),
        }
    }

    fn with_response_headers(
        mut self,
        headers: Vec<(impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.response_headers = headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        self
    }
}

struct RecordedHttpCall {
    method: Method,
    uri: String,
    backend_name: String,
}

struct FixedBackend;

impl PlatformBackend for FixedBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok(format!("{}-{}", spec.scheme, spec.host))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for RecordingHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.calls
            .lock()
            .expect("should lock calls")
            .push(RecordedHttpCall {
                method: request.request.method().clone(),
                uri: request.request.uri().to_string(),
                backend_name: request.backend_name,
            });

        let mut builder = edge_response_builder().status(self.response_status);
        for (name, value) in &self.response_headers {
            builder = builder.header(name, value);
        }
        let edge_response = builder
            .body(EdgeBody::from(Vec::new()))
            .map_err(|_| Report::new(PlatformError::HttpClient))?;

        Ok(PlatformResponse::new(edge_response))
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
    test_runtime_services_with_http_client(
        req,
        Arc::new(NoopBackend),
        Arc::new(NoopHttpClient) as Arc<dyn PlatformHttpClient>,
    )
}

fn admin_authorization_header() -> String {
    format!("Basic {}", STANDARD.encode("admin:admin-pass"))
}

fn test_runtime_services_with_http_client(
    req: &Request,
    backend: Arc<dyn PlatformBackend>,
    http_client: Arc<dyn PlatformHttpClient>,
) -> RuntimeServices {
    test_runtime_services_with_secret_and_http_client(
        req,
        backend,
        Arc::new(NoopSecretStore),
        http_client,
    )
}

fn test_runtime_services_with_secret_and_http_client(
    req: &Request,
    backend: Arc<dyn PlatformBackend>,
    secret_store: Arc<dyn PlatformSecretStore>,
    http_client: Arc<dyn PlatformHttpClient>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(StubJwksConfigStore))
        .secret_store(secret_store)
        .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
        .backend(backend)
        .http_client(http_client)
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

fn route_buffered_response(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    req: Request,
    expect_message: &str,
) -> fastly::Response {
    let partner_registry =
        PartnerRegistry::from_config(&settings.ec.partners).expect("should build partner registry");

    futures::executor::block_on(route_request(
        settings,
        orchestrator,
        integration_registry,
        &partner_registry,
        services,
        req,
    ))
    .expect(expect_message)
    .response
    .expect("should buffer route response in tests")
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

#[test]
fn admin_s3_debug_requires_auth_even_when_disabled() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/admin/debug/s3-objects");
    let services = test_runtime_services(&req);
    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route admin S3 debug request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::UNAUTHORIZED,
        "should challenge unauthenticated S3 debug requests"
    );
}

#[test]
fn admin_s3_debug_disabled_returns_404_after_auth() {
    let mut settings = create_test_settings();
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/admin/debug/s3-objects")
        .with_header(header::AUTHORIZATION, admin_authorization_header());
    let services = test_runtime_services(&req);
    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route admin S3 debug request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::NOT_FOUND,
        "should hide the S3 debug endpoint when the config flag is disabled"
    );
    assert_eq!(
        resp.get_header_str(header::CACHE_CONTROL),
        Some("no-store, private"),
        "should prevent configured response headers from making S3 debug responses cacheable"
    );
}

#[test]
fn asset_routes_bypass_publisher_consent_dependencies() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let asset_req = Request::get("https://test.com/.images/logo.png?auto=webp");
    let asset_services = test_runtime_services(&asset_req);
    let asset_resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &asset_services,
        asset_req,
        "should return an error response for asset proxy requests",
    );
    assert_eq!(
        asset_resp.get_status(),
        StatusCode::BAD_GATEWAY,
        "should bypass publisher consent dependencies and fail only on the missing upstream client"
    );
}

#[test]
fn asset_origin_failure_does_not_fall_back_to_publisher_origin() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.images/logo.png");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::OK));
    let services = test_runtime_services_with_http_client(
        &req,
        Arc::new(NoopBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should return an error response for failed asset origin",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::BAD_GATEWAY,
        "should stop asset-origin backend failures at the asset proxy path"
    );
    assert!(
        http_client
            .calls
            .lock()
            .expect("should lock recorded calls")
            .is_empty(),
        "should not invoke the publisher origin when asset backend registration fails"
    );
}

#[test]
fn s3_asset_origin_error_stays_uncacheable_after_global_headers() {
    let mut settings = create_test_settings();
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    settings.image_optimizer = ImageOptimizerSettings {
        profile_sets: HashMap::from([(
            "default_images".to_string(),
            ImageOptimizerProfileSet {
                base_params: String::new(),
                default_profile: "default".to_string(),
                unknown_profile: Default::default(),
                profile_param: "profile".to_string(),
                aspect_ratio_param: "ar".to_string(),
                debug_param: "_io_debug".to_string(),
                profiles: HashMap::from([("default".to_string(), "width=100".to_string())]),
                aspect_ratios: None,
                crop_offsets: None,
            },
        )]),
    };
    let mut route = ProxyAssetRoute::new(
        "/.images/",
        "https://examplebucket.s3.us-east-1.amazonaws.com",
    );
    route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
        region: "us-east-1".to_string(),
        secret_store: "s3-auth".to_string(),
        access_key_id: "access_key_id".to_string(),
        secret_access_key: "secret_access_key".to_string(),
        session_token: None,
        origin_query: None,
    }));
    route.image_optimizer = Some(AssetImageOptimizerConfig {
        enabled: true,
        region: "us_east".to_string(),
        profile_set: "default_images".to_string(),
        origin_query: None,
    });
    settings.proxy.asset_routes = vec![route];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.images/missing.png?profile=default");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::NOT_FOUND));
    let services = test_runtime_services_with_secret_and_http_client(
        &req,
        Arc::new(FixedBackend),
        Arc::new(HashMapSecretStore::new(HashMap::from([
            (
                "access_key_id".to_string(),
                b"AKIAIOSFODNN7EXAMPLE".to_vec(),
            ),
            (
                "secret_access_key".to_string(),
                b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_vec(),
            ),
        ]))),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
    ))
    .expect("should route S3 asset request");

    assert_eq!(
        resp.get_status(),
        StatusCode::NOT_FOUND,
        "should return the raw S3 origin error status"
    );
    assert_eq!(
        resp.get_header_str(header::CACHE_CONTROL),
        Some("no-store, private"),
        "should preserve the S3 origin error no-store policy after global headers"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(
        calls.len(),
        2,
        "should preflight with HEAD and then fetch the S3 error body"
    );
}

#[test]
fn asset_routes_proxy_head_requests() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::head("https://test.com/.images/logo.png");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::NO_CONTENT));
    let services = test_runtime_services_with_http_client(
        &req,
        Arc::new(FixedBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route HEAD asset request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::NO_CONTENT,
        "should pass through asset-origin HEAD response status"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should send exactly one asset request");
    assert_eq!(
        calls[0].method,
        Method::HEAD,
        "should forward HEAD upstream"
    );
    assert!(
        calls[0].backend_name.contains("assets.example.com"),
        "should send to the asset backend, got {}",
        calls[0].backend_name
    );
}

#[test]
fn asset_routes_ignore_query_string_for_matching() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.images/logo.png?auto=webp");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::OK));
    let services = test_runtime_services_with_http_client(
        &req,
        Arc::new(FixedBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route asset request with query string",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::OK,
        "should match by path only"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should send exactly one asset request");
    assert!(
        calls[0].uri.ends_with("/.images/logo.png?auto=webp"),
        "should preserve query on the upstream asset request, got {}",
        calls[0].uri
    );
}

#[test]
fn asset_routes_pass_redirect_responses_through() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.images/logo.png");
    let http_client = Arc::new(
        RecordingHttpClient::new(StatusCode::FOUND).with_response_headers(vec![(
            header::LOCATION.as_str(),
            "https://cdn.example.com/logo.png",
        )]),
    );
    let services = test_runtime_services_with_http_client(
        &req,
        Arc::new(FixedBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route redirecting asset request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::FOUND,
        "should pass redirect status through without following it"
    );
    assert_eq!(
        resp.get_header_str(header::LOCATION),
        Some("https://cdn.example.com/logo.png"),
        "should preserve asset-origin redirect location"
    );
}

#[test]
fn asset_routes_skip_non_get_head_requests() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::post("https://test.com/.images/logo.png");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::OK));
    let services = test_runtime_services_with_http_client(
        &req,
        Arc::new(FixedBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route non-asset POST request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "should fall through to publisher fallback for POST requests"
    );
    assert!(
        http_client
            .calls
            .lock()
            .expect("should lock recorded calls")
            .is_empty(),
        "should not send POST requests through asset routing"
    );
}

#[test]
fn built_in_routes_take_precedence_over_asset_routes() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.well-known/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.well-known/trusted-server.json");
    let services = test_runtime_services(&req);
    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route discovery request",
    );
    assert_eq!(
        resp.get_status(),
        StatusCode::OK,
        "should keep explicit built-in routes ahead of asset routes"
    );
}

#[test]
fn integration_routes_take_precedence_over_asset_routes() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/prebid.js",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/prebid.js");
    let services = test_runtime_services(&req);
    let mut resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route integration request",
    );
    assert_eq!(
        resp.get_status(),
        StatusCode::OK,
        "should keep explicit integration routes ahead of asset routes"
    );
    assert_eq!(
        resp.take_body_str(),
        "// Script overridden by Trusted Server\n",
        "should serve the integration response instead of proxying to the asset origin"
    );
}
