use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::response_builder as edge_response_builder;
use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::http::{header, Method, StatusCode};
use fastly::Request;
use serde_json::json;
use trusted_server_core::auction::{
    build_orchestrator, AuctionContext, AuctionOrchestrator, AuctionProvider, AuctionRequest,
    AuctionResponse,
};
use trusted_server_core::compat;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};
use trusted_server_core::proxy::AssetProxyCachePolicy;
use trusted_server_core::request_signing::JWKS_CONFIG_STORE_NAME;
use trusted_server_core::settings::{
    AssetImageOptimizerConfig, AssetOriginAuth, ImageOptimizerProfileSet, ImageOptimizerSettings,
    ProxyAssetRoute, S3SigV4AuthConfig, Settings,
};

use super::{route_request, HandlerOutcome};

#[test]
fn streaming_publisher_path_uses_async_auction_collector() {
    let router_source = include_str!("main.rs");

    assert!(
        router_source.contains("stream_publisher_body_async("),
        "streaming publisher responses must collect dispatched auctions before </body> injection"
    );
}

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
    response_body: Vec<u8>,
}

struct StreamingRecordingHttpClient {
    calls: Mutex<Vec<RecordedHttpCall>>,
}

impl RecordingHttpClient {
    fn new(response_status: StatusCode) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response_status,
            response_headers: Vec::new(),
            response_body: Vec::new(),
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

    fn with_response_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.response_body = body.into();
        self
    }
}

impl StreamingRecordingHttpClient {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

struct RecordedHttpCall {
    method: Method,
    uri: String,
    backend_name: String,
    stream_response: bool,
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
                stream_response: request.stream_response,
            });

        let mut builder = edge_response_builder().status(self.response_status);
        for (name, value) in &self.response_headers {
            builder = builder.header(name, value);
        }
        let edge_response = builder
            .body(EdgeBody::from(self.response_body.clone()))
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
impl PlatformHttpClient for StreamingRecordingHttpClient {
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
                stream_response: request.stream_response,
            });

        let edge_response = edge_response_builder()
            .status(StatusCode::OK)
            .body(EdgeBody::stream(futures::stream::iter(vec![
                Bytes::from_static(b"streamed-asset"),
            ])))
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

#[async_trait::async_trait(?Send)]
impl AuctionProvider for DisabledRouteProvider {
    fn provider_name(&self) -> &'static str {
        "disabled-route"
    }

    async fn request_bids(
        &self,
        _request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Auction {
            message: "disabled route provider should not launch requests".to_string(),
        }))
    }

    async fn parse_response(
        &self,
        _response: PlatformResponse,
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

struct FixedGeo(GeoInfo);

impl PlatformGeo for FixedGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(self.0.clone()))
    }
}

fn us_california_geo() -> GeoInfo {
    GeoInfo {
        city: "Example City".to_string(),
        country: "US".to_string(),
        continent: "NA".to_string(),
        latitude: 37.0,
        longitude: -122.0,
        metro_code: 807,
        region: Some("CA".to_string()),
        asn: None,
    }
}

/// Geo resolving to a non-regulated jurisdiction, so the server-side auction
/// consent gate (which fails closed for GDPR/unknown jurisdictions without TCF
/// Purpose 1) allows the auction to proceed. Used by `/auction` route tests
/// that exercise orchestration behavior rather than consent.
fn non_regulated_geo() -> GeoInfo {
    GeoInfo {
        city: "Example City".to_string(),
        country: "AU".to_string(),
        continent: "OC".to_string(),
        latitude: -33.8,
        longitude: 151.2,
        metro_code: 0,
        region: Some("NSW".to_string()),
        asn: None,
    }
}

fn valid_ec_id() -> String {
    format!("{}.Abc123", "a".repeat(64))
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

fn datadome_protection_toml() -> &'static str {
    r#"
            [integrations.datadome]
            enabled = true
            enable_protection = true
            server_side_key_secret_store = "ts_secrets"
            server_side_key_secret_name = "datadome_server_side_key"
        "#
}

fn create_datadome_auction_test_settings(providers: &str) -> Settings {
    let base = base_route_settings_toml();
    let datadome = datadome_protection_toml();
    let config = format!(
        r#"{base}

{datadome}

            [auction]
            enabled = true
            providers = {providers}
            timeout_ms = 2000
        "#,
    );

    Settings::from_toml(&config).expect("should parse DataDome route test settings")
}

fn datadome_secret_store() -> Arc<dyn PlatformSecretStore> {
    Arc::new(HashMapSecretStore::new(HashMap::from([(
        "datadome_server_side_key".to_string(),
        b"datadome-server-side-key".to_vec(),
    )])))
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
    test_runtime_services_with_secret_http_client_and_geo(
        req,
        backend,
        secret_store,
        http_client,
        Arc::new(NoopGeo),
    )
}

fn test_runtime_services_with_secret_http_client_and_geo(
    req: &Request,
    backend: Arc<dyn PlatformBackend>,
    secret_store: Arc<dyn PlatformSecretStore>,
    http_client: Arc<dyn PlatformHttpClient>,
    geo: Arc<dyn PlatformGeo>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(StubJwksConfigStore))
        .secret_store(secret_store)
        .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
        .backend(backend)
        .http_client(http_client)
        .geo(geo)
        .client_info(ClientInfo {
            client_ip: req.get_client_ip_addr(),
            tls_protocol: req.get_tls_protocol().map(str::to_string),
            tls_cipher: req.get_tls_cipher_openssl_name().map(str::to_string),
            tls_ja4: req.get_tls_ja4().map(str::to_string),
            h2_fingerprint: req.get_client_h2_fingerprint().map(str::to_string),
            server_hostname: None,
            server_region: None,
        })
        .build()
}

fn test_partner_registry(settings: &Settings) -> PartnerRegistry {
    PartnerRegistry::from_config(&settings.ec.partners).expect("should build partner registry")
}

fn route_result_to_fastly_response(
    settings: &Settings,
    services: &RuntimeServices,
    partner_registry: &PartnerRegistry,
    route_result: super::RouteResult,
) -> fastly::Response {
    let super::RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        should_finalize_ec,
        asset_cache_policy,
        request_filter_effects,
        ..
    } = route_result;

    let is_auth_challenge = matches!(&outcome, HandlerOutcome::AuthChallenge(_));
    let mut response = match outcome {
        HandlerOutcome::Buffered(response) | HandlerOutcome::AuthChallenge(response) => {
            Some(response)
        }
        _ => None,
    }
    .expect("should have a buffered route response");

    let geo_info = if is_auth_challenge {
        None
    } else {
        services
            .geo()
            .lookup(services.client_info().client_ip)
            .unwrap_or(None)
    };
    super::finalize_response(settings, geo_info.as_ref(), &mut response);
    asset_cache_policy.apply_after_route_finalization(&mut response);

    let mut fastly_response = compat::to_fastly_response(response);
    if should_finalize_ec {
        ec_finalize_response(
            settings,
            &ec_context,
            finalize_kv_graph.as_ref(),
            partner_registry,
            eids_cookie.as_deref(),
            sharedid_cookie.as_deref(),
            &mut fastly_response,
        );
    }
    // Mirror main's ordering: apply request-filter response effects (which may
    // append a per-user Set-Cookie) before the final cache guard so the guard
    // observes them.
    request_filter_effects.apply_to_fastly_response(&mut fastly_response);
    super::enforce_set_cookie_cache_privacy(&mut fastly_response);
    fastly_response
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
    // Resolve to a non-regulated jurisdiction so the server-side auction consent
    // gate allows the auction; these tests assert orchestration behavior, not
    // consent gating (covered separately in endpoints.rs).
    let services = test_runtime_services_with_secret_http_client_and_geo(
        &req,
        Arc::new(NoopBackend),
        Arc::new(NoopSecretStore),
        Arc::new(NoopHttpClient) as Arc<dyn PlatformHttpClient>,
        Arc::new(FixedGeo(non_regulated_geo())),
    );

    let route_result = futures::executor::block_on(route_request(
        settings,
        orchestrator,
        integration_registry,
        &partner_registry,
        &services,
        &[],
        compat::from_fastly_request(req),
    ))
    .expect("should route auction request");
    route_result_to_fastly_response(settings, &services, &partner_registry, route_result)
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

    let route_result = futures::executor::block_on(route_request(
        settings,
        orchestrator,
        integration_registry,
        &partner_registry,
        services,
        &[],
        compat::from_fastly_request(req),
    ))
    .expect(expect_message);
    route_result_to_fastly_response(settings, services, &partner_registry, route_result)
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
fn datadome_challenge_short_circuits_before_publisher_origin() {
    let settings = create_datadome_auction_test_settings("[]");
    let (orchestrator, integration_registry) = build_route_stack(&settings);
    let req = Request::get("https://test.com/protected-page");
    let http_client = Arc::new(
        RecordingHttpClient::new(StatusCode::FORBIDDEN)
            .with_response_headers(vec![
                ("x-datadomeresponse", "403"),
                ("x-datadome-headers", "Set-Cookie X-DD-B"),
                ("set-cookie", "datadome=challenge; Path=/; HttpOnly"),
                ("x-dd-b", "1"),
            ])
            .with_response_body(b"blocked by datadome".to_vec()),
    );
    let services = test_runtime_services_with_secret_and_http_client(
        &req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let mut response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route DataDome challenge response",
    );

    assert_eq!(
        response.get_status(),
        StatusCode::FORBIDDEN,
        "should return the DataDome challenge status instead of contacting publisher origin"
    );
    assert_eq!(
        response.get_header_str("x-dd-b"),
        Some("1"),
        "should apply DataDome downstream challenge headers"
    );
    assert_eq!(
        response.get_header_str(header::SET_COOKIE),
        Some("datadome=challenge; Path=/; HttpOnly"),
        "should append the DataDome challenge cookie"
    );
    assert_eq!(
        response.take_body_str(),
        "blocked by datadome",
        "should return the DataDome challenge body"
    );

    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should call only the Protection API");
    assert_eq!(calls[0].method, Method::POST, "should POST to DataDome");
    assert_eq!(
        calls[0].uri, "https://api-fastly.datadome.co/validate-request",
        "should call the default DataDome Protection API endpoint"
    );
}

#[test]
fn datadome_allow_applies_downstream_headers_and_protects_auction() {
    let settings = create_datadome_auction_test_settings("[]");
    let (orchestrator, integration_registry) = build_route_stack(&settings);
    let req = Request::post("https://test.com/auction")
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_body(valid_banner_ad_unit_body());
    let http_client = Arc::new(
        RecordingHttpClient::new(StatusCode::OK).with_response_headers(vec![
            ("x-datadomeresponse", "200"),
            ("x-datadome-headers", "Set-Cookie X-DD-B"),
            ("set-cookie", "datadome=allow; Path=/; HttpOnly"),
            ("x-dd-b", "allowed"),
        ]),
    );
    // Resolve to a non-regulated jurisdiction so the server-side auction consent
    // gate allows the auction to run; this test asserts DataDome protection plus
    // auction orchestration, not consent gating (covered in endpoints.rs).
    let services = test_runtime_services_with_secret_http_client_and_geo(
        &req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
        Arc::new(FixedGeo(non_regulated_geo())),
    );

    let response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route DataDome-allowed auction request",
    );

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_GATEWAY,
        "empty-provider auction should still run after DataDome allows the request"
    );
    assert_eq!(
        response.get_header_str("x-dd-b"),
        Some("allowed"),
        "should apply DataDome downstream headers after route finalization"
    );
    assert_eq!(
        response.get_header_str(header::SET_COOKIE),
        Some("datadome=allow; Path=/; HttpOnly"),
        "should preserve DataDome downstream Set-Cookie on allowed requests"
    );

    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(
        calls.len(),
        1,
        "should protect /auction through DataDome by default"
    );
    assert_eq!(calls[0].method, Method::POST, "should POST to DataDome");
}

#[test]
fn datadome_api_error_fails_open_before_routing() {
    let settings = create_datadome_auction_test_settings("[]");
    let (orchestrator, integration_registry) = build_route_stack(&settings);
    let req = Request::post("https://test.com/auction")
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_body(b"{not-json".to_vec());
    let services = test_runtime_services_with_secret_and_http_client(
        &req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::new(NoopHttpClient) as Arc<dyn PlatformHttpClient>,
    );

    let response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should fail open when DataDome API call fails",
    );

    assert_eq!(
        response.get_status(),
        StatusCode::BAD_REQUEST,
        "malformed auction JSON should be handled by the route after DataDome fails open"
    );
    assert_eq!(
        response.get_header_str("x-dd-b"),
        None,
        "should not apply DataDome headers when the Protection API call fails"
    );
}

#[test]
fn datadome_skips_internal_and_static_asset_routes_by_default() {
    let mut settings = create_datadome_auction_test_settings("[]");
    settings.publisher.origin_url = "https://".to_string();
    let (orchestrator, integration_registry) = build_route_stack(&settings);
    let http_client = Arc::new(
        RecordingHttpClient::new(StatusCode::OK).with_response_headers(vec![
            ("x-datadomeresponse", "200"),
            ("x-datadome-headers", "X-DD-B"),
            ("x-dd-b", "should-not-apply"),
        ]),
    );

    let discovery_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services_with_secret_and_http_client(
        &discovery_req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );
    let discovery_response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &discovery_services,
        discovery_req,
        "should route internal discovery request without DataDome",
    );
    assert_eq!(
        discovery_response.get_status(),
        StatusCode::OK,
        "discovery endpoint should stay internal"
    );

    let image_req = Request::get("https://test.com/logo.png");
    let image_services = test_runtime_services_with_secret_and_http_client(
        &image_req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );
    let image_response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &image_services,
        image_req,
        "should route static asset request without DataDome",
    );
    assert_eq!(
        image_response.get_status(),
        StatusCode::BAD_GATEWAY,
        "static asset should skip DataDome then fail at the intentionally invalid publisher origin"
    );

    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert!(
        calls.is_empty(),
        "should not call DataDome for internal routes or default-excluded static assets"
    );
}

#[test]
fn datadome_skips_registered_integration_routes_with_custom_prefix() {
    let base = base_route_settings_toml();
    let datadome = datadome_protection_toml();
    let config = format!(
        r#"{base}

{datadome}

            [integrations.didomi]
            enabled = true
            proxy_path = "my-consent"
            sdk_origin = "https://sdk.privacy-center.org"
            api_origin = "https://api.privacy-center.org"

            [auction]
            enabled = true
            providers = []
            timeout_ms = 2000
        "#,
    );
    let settings = Settings::from_toml(&config)
        .expect("should parse DataDome and custom Didomi route test settings");
    let (orchestrator, integration_registry) = build_route_stack(&settings);
    let req = Request::get("https://test.com/my-consent/notice");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::OK));
    let services = test_runtime_services_with_secret_and_http_client(
        &req,
        Arc::new(FixedBackend),
        datadome_secret_store(),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );

    let response = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route custom Didomi proxy request without DataDome",
    );

    assert_eq!(
        response.get_status(),
        StatusCode::OK,
        "custom integration proxy route should still be handled"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should call only the Didomi upstream");
    assert_eq!(
        calls[0].uri, "https://sdk.privacy-center.org/notice",
        "should not call the DataDome Protection API for registered integration routes"
    );
}

#[test]
fn routes_use_request_local_consent() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let partner_registry = test_partner_registry(&settings);

    let discovery_fastly_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services(&discovery_fastly_req);
    let discovery_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &discovery_services,
        &[],
        compat::from_fastly_request(discovery_fastly_req),
    ))
    .expect("should route discovery request");
    assert_eq!(
        discovery_resp.outcome.status(),
        StatusCode::OK,
        "should keep discovery available with request-local consent"
    );

    let admin_fastly_req = Request::post("https://test.com/_ts/admin/keys/rotate");
    let admin_services = test_runtime_services(&admin_fastly_req);
    let admin_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &admin_services,
        &[],
        compat::from_fastly_request(admin_fastly_req),
    ))
    .expect("should route admin request");
    assert_eq!(
        admin_resp.outcome.status(),
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
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::ACCEPTED));
    let asset_services = test_runtime_services_with_http_client(
        &asset_req,
        Arc::new(FixedBackend),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
    );
    let asset_resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &asset_services,
        asset_req,
        "should route asset proxy request",
    );

    assert_eq!(
        asset_resp.get_status(),
        StatusCode::ACCEPTED,
        "should return the asset-origin response without publisher consent dependencies"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should send exactly one asset request");
    assert_eq!(
        calls[0].backend_name, "https-assets.example.com",
        "should resolve the configured asset backend, not the publisher origin"
    );
    assert_eq!(
        calls[0].uri, "https://assets.example.com/.images/logo.png?auto=webp",
        "should send the request to the configured asset origin"
    );
}

#[test]
fn asset_routes_skip_ec_finalization_cookie_mutations() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let mut req = Request::get("https://test.com/.images/logo.png");
    req.set_header(header::COOKIE, format!("ts-ec={}", valid_ec_id()));
    req.set_header("sec-gpc", "1");
    let http_client = Arc::new(RecordingHttpClient::new(StatusCode::OK));
    let services = test_runtime_services_with_secret_http_client_and_geo(
        &req,
        Arc::new(FixedBackend),
        Arc::new(NoopSecretStore),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
        Arc::new(FixedGeo(us_california_geo())),
    );

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route asset request without EC finalization",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::OK,
        "should return the asset-origin response"
    );
    assert_eq!(
        resp.get_header_str(header::SET_COOKIE),
        None,
        "should not expire or set EC cookies on asset responses"
    );
    assert_eq!(
        resp.get_header_str("x-ts-ec"),
        None,
        "should not emit EC identity headers on asset responses"
    );
}

#[test]
fn asset_routes_stream_asset_responses_directly() {
    let mut settings = create_test_settings();
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let partner_registry = test_partner_registry(&settings);

    let mut fastly_req = Request::get("https://test.com/.images/logo.png");
    fastly_req.set_header(header::COOKIE, format!("ts-ec={}", valid_ec_id()));
    fastly_req.set_header("sec-gpc", "1");
    let http_client = Arc::new(StreamingRecordingHttpClient::new());
    let services = test_runtime_services_with_secret_http_client_and_geo(
        &fastly_req,
        Arc::new(FixedBackend),
        Arc::new(NoopSecretStore),
        Arc::clone(&http_client) as Arc<dyn PlatformHttpClient>,
        Arc::new(FixedGeo(us_california_geo())),
    );
    let req = compat::from_fastly_request(fastly_req);

    let outcome = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &services,
        &[],
        req,
    ))
    .expect("should route streaming asset request");

    assert!(
        !outcome.should_finalize_ec,
        "asset routes should not emit EC identity headers"
    );
    assert_eq!(
        outcome.asset_cache_policy,
        AssetProxyCachePolicy::OriginControlled,
        "successful asset routes should preserve origin cache policy"
    );
    let (response, body) = match outcome.outcome {
        HandlerOutcome::AssetStreaming { response, body } => Some((response, body)),
        _ => None,
    }
    .expect("should return streaming asset outcome");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "should preserve streaming asset response status"
    );
    assert!(
        matches!(body, EdgeBody::Stream(_)),
        "should preserve streaming asset response body"
    );
    let calls = http_client
        .calls
        .lock()
        .expect("should lock recorded calls");
    assert_eq!(calls.len(), 1, "should send exactly one asset request");
    assert!(
        calls[0].stream_response,
        "asset routes should request a streaming origin response from the platform"
    );
    assert_eq!(
        calls[0].backend_name, "https-assets.example.com",
        "should resolve the configured asset backend, not the publisher origin"
    );
    assert_eq!(
        calls[0].uri, "https://assets.example.com/.images/logo.png",
        "should send the request to the configured asset origin"
    );
    // `stream_to_client` commits headers and bytes into the Fastly host runtime,
    // leaving no buffered `Response` for this route-level test to inspect. Core
    // proxy tests assert unsafe header stripping on the real streaming body; this
    // test pins the adapter contract that successful streaming asset responses
    // take the `response: None` direct-send path with EC finalization skipped.
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
fn asset_handler_error_stays_uncacheable_after_global_headers() {
    let mut settings = create_test_settings();
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    settings.proxy.asset_routes = vec![ProxyAssetRoute::new(
        "/.images/",
        "https://assets.example.com",
    )];
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/.images/logo.png");
    let services = test_runtime_services(&req);

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route generated asset error response",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::BAD_GATEWAY,
        "should return the generated asset proxy error"
    );
    assert_eq!(
        resp.get_header_str(header::CACHE_CONTROL),
        Some("no-store, private"),
        "should not let global cache headers make generated asset errors cacheable"
    );
}

#[test]
fn finalize_response_preserves_origin_cache_headers_for_plain_html() {
    // Reviewer P1.2: a zero-slot / non-matching navigation injects no per-user
    // data and sets no cookie, so the publisher path leaves Cache-Control alone.
    // finalize_response must not downgrade shared cacheability for it.
    let settings = create_test_settings();
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header("surrogate-control", "max-age=86400")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=3600"),
        "plain cookieless HTML should keep its shared cache directive"
    );
    assert_eq!(
        response
            .headers()
            .get("surrogate-control")
            .and_then(|v| v.to_str().ok()),
        Some("max-age=86400"),
        "plain cookieless HTML should keep its surrogate cache directive"
    );
}

#[test]
fn finalize_response_makes_cookie_bearing_responses_private() {
    // A first-visit navigation that only sets the EC identity cookie must not be
    // shared-cached, even though no ad data was injected.
    let settings = create_test_settings();
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header("surrogate-control", "max-age=86400")
        .header(header::SET_COOKIE, "ec=abc; Path=/; HttpOnly")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("private, max-age=0"),
        "a Set-Cookie response must be downgraded to private"
    );
    assert_eq!(
        response
            .headers()
            .get("surrogate-control")
            .and_then(|v| v.to_str().ok()),
        None,
        "a Set-Cookie response must not retain surrogate cacheability"
    );
}

#[test]
fn ec_set_cookie_added_after_finalize_downgrades_origin_public_cache() {
    // First-visit navigation: the origin response is shared-cacheable and carries
    // no cookie, so the HttpResponse-stage finalizer keeps its cache headers. EC
    // finalization then mints the identity Set-Cookie on the converted Fastly
    // response, after that guard has already run. The post-EC privacy guard must
    // downgrade caching so a shared cache cannot replay one visitor's EC cookie.
    let settings = create_test_settings();
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header("surrogate-control", "max-age=86400")
        .body(EdgeBody::empty())
        .expect("should build test response");

    // No cookie at this stage, so the cookie net does not fire and the origin
    // cache directive survives finalize_response — reproducing the gap.
    super::finalize_response(&settings, None, &mut response);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=3600"),
        "a cookieless response should keep its origin cache directive"
    );

    let mut fastly_response = compat::to_fastly_response(response);
    // Stand in for ec_finalize_response minting the first-visit identity cookie:
    // its EcContext constructors are #[cfg(test)] in trusted-server-core and are
    // not reachable from this crate, but the only behavior under test here is the
    // post-EC ordering — a Set-Cookie appearing after finalize_response ran.
    fastly_response.set_header(header::SET_COOKIE, "ec=abc; Path=/; HttpOnly");

    super::enforce_set_cookie_cache_privacy(&mut fastly_response);

    assert_eq!(
        fastly_response.get_header_str("cache-control"),
        Some("private, max-age=0"),
        "an EC Set-Cookie added after finalize_response must downgrade caching"
    );
    assert!(
        fastly_response.get_header("surrogate-control").is_none(),
        "EC Set-Cookie responses must not retain surrogate cacheability"
    );
}

#[test]
fn enforce_set_cookie_cache_privacy_keeps_stricter_no_store() {
    // A stricter directive minted alongside the cookie must not be weakened to
    // the `private, max-age=0` downgrade.
    let mut fastly_response = compat::to_fastly_response(
        edge_response_builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::SET_COOKIE, "ec=abc; Path=/")
            .body(EdgeBody::empty())
            .expect("should build test response"),
    );

    super::enforce_set_cookie_cache_privacy(&mut fastly_response);

    assert_eq!(
        fastly_response.get_header_str("cache-control"),
        Some("no-store"),
        "an already-uncacheable response should keep its stricter directive"
    );
}

#[test]
fn enforce_set_cookie_cache_privacy_strips_surrogate_on_no_store() {
    // A `no-store` cookie response keeps its stricter Cache-Control but must still
    // lose the surrogate cache headers — they are independent of Cache-Control and
    // would otherwise let a shared cache store and replay the visitor's cookie.
    let mut fastly_response = compat::to_fastly_response(
        edge_response_builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "no-store")
            .header("surrogate-control", "max-age=86400")
            .header("fastly-surrogate-control", "max-age=86400")
            .header(header::SET_COOKIE, "ec=abc; Path=/")
            .body(EdgeBody::empty())
            .expect("should build test response"),
    );

    super::enforce_set_cookie_cache_privacy(&mut fastly_response);

    assert_eq!(
        fastly_response.get_header_str("cache-control"),
        Some("no-store"),
        "should keep the stricter no-store directive"
    );
    assert!(
        fastly_response.get_header("surrogate-control").is_none(),
        "no-store cookie responses must not retain Surrogate-Control"
    );
    assert!(
        fastly_response
            .get_header("fastly-surrogate-control")
            .is_none(),
        "no-store cookie responses must not retain Fastly-Surrogate-Control"
    );
}

#[test]
fn request_filter_set_cookie_after_guard_still_downgrades_cache() {
    // A request filter (e.g. a DataDome allow) can append a per-user Set-Cookie via
    // response effects. main applies those effects before the final cache guard, so
    // an origin response still marked `public` with surrogate headers must be
    // downgraded once the filter cookie is present.
    use trusted_server_core::integrations::{HeaderMutation, RequestFilterEffects};

    let mut fastly_response = compat::to_fastly_response(
        edge_response_builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .header("surrogate-control", "max-age=86400")
            .body(EdgeBody::empty())
            .expect("should build test response"),
    );

    let effects = RequestFilterEffects {
        request_headers: vec![],
        response_headers: vec![HeaderMutation::append(
            "set-cookie",
            "datadome=allow; Path=/; HttpOnly",
        )],
    };

    // Mirror main's ordering: apply effects first, then the guard.
    effects.apply_to_fastly_response(&mut fastly_response);
    super::enforce_set_cookie_cache_privacy(&mut fastly_response);

    assert_eq!(
        fastly_response.get_header_str("cache-control"),
        Some("private, max-age=0"),
        "a filter-added Set-Cookie must downgrade a public origin response"
    );
    assert!(
        fastly_response.get_header("surrogate-control").is_none(),
        "a filter-added Set-Cookie must strip surrogate cacheability"
    );
}

#[test]
fn finalize_response_no_store_cookie_blocks_operator_surrogate_reenable() {
    // Operator response_headers must not re-add surrogate caching to a Set-Cookie
    // response carrying the stricter `no-store` directive — the operator guard must
    // treat no-store as protected, not just `private`.
    let mut settings = create_test_settings();
    settings
        .response_headers
        .insert("Surrogate-Control".to_string(), "max-age=86400".to_string());
    settings.response_headers.insert(
        "Fastly-Surrogate-Control".to_string(),
        "max-age=86400".to_string(),
    );
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::SET_COOKIE, "ec=abc; Path=/")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "operator Cache-Control must not weaken the stricter no-store directive"
    );
    assert_eq!(
        response
            .headers()
            .get("surrogate-control")
            .and_then(|v| v.to_str().ok()),
        None,
        "operator Surrogate-Control must not re-enable caching for a no-store cookie response"
    );
    assert_eq!(
        response
            .headers()
            .get("fastly-surrogate-control")
            .and_then(|v| v.to_str().ok()),
        None,
        "operator Fastly-Surrogate-Control must not re-enable caching for a no-store cookie response"
    );
}

#[test]
fn finalize_response_leaves_stricter_no_store_untouched() {
    let settings = create_test_settings();
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::SET_COOKIE, "ec=abc; Path=/")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "an already-uncacheable response should keep its stricter directive"
    );
}

#[test]
fn finalize_response_treats_mixed_case_no_store_as_uncacheable() {
    // Cache-Control directives are case-insensitive: `No-Store` on a Set-Cookie
    // response must be recognized as already-uncacheable and left untouched, not
    // downgraded to the weaker `private, max-age=0`.
    let settings = create_test_settings();
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, "No-Store")
        .header(header::SET_COOKIE, "ec=abc; Path=/")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("No-Store"),
        "mixed-case No-Store must be treated as uncacheable and preserved"
    );
}

#[test]
fn finalize_response_mixed_case_private_blocks_operator_surrogate_reenable() {
    // A mixed-case `Private` directive must still mark the response private so
    // operator response_headers cannot re-enable shared caching.
    let mut settings = create_test_settings();
    settings
        .response_headers
        .insert("Surrogate-Control".to_string(), "max-age=86400".to_string());
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "Private, max-age=0")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get("surrogate-control")
            .and_then(|v| v.to_str().ok()),
        None,
        "operator Surrogate-Control must not re-enable caching for a mixed-case Private response"
    );
}

#[test]
fn finalize_response_cookie_net_blocks_operator_surrogate_reenable() {
    // Operator response_headers must not re-add surrogate caching once the
    // cookie net has marked the response private.
    let mut settings = create_test_settings();
    settings
        .response_headers
        .insert("Surrogate-Control".to_string(), "max-age=86400".to_string());
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    let mut response = edge_response_builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::SET_COOKIE, "ec=abc; Path=/")
        .body(EdgeBody::empty())
        .expect("should build test response");

    super::finalize_response(&settings, None, &mut response);

    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("private, max-age=0"),
        "operator Cache-Control must not override the cookie privacy directive"
    );
    assert_eq!(
        response
            .headers()
            .get("surrogate-control")
            .and_then(|v| v.to_str().ok()),
        None,
        "operator Surrogate-Control must not re-enable shared caching for a cookie response"
    );
}

#[test]
fn page_bids_response_cannot_regain_surrogate_headers_from_settings() {
    let base = base_route_settings_toml();
    let prebid = prebid_integration_toml();
    let config = format!(
        r#"{base}

{prebid}

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000

            [creative_opportunities]
            gam_network_id = "1234"
        "#,
    );
    let mut settings =
        Settings::from_toml(&config).expect("should parse page-bids route test settings");
    settings
        .response_headers
        .insert("Surrogate-Control".to_string(), "max-age=86400".to_string());
    settings.response_headers.insert(
        "Fastly-Surrogate-Control".to_string(),
        "max-age=86400".to_string(),
    );
    settings.response_headers.insert(
        header::CACHE_CONTROL.as_str().to_string(),
        "public, max-age=3600".to_string(),
    );
    let (orchestrator, integration_registry) = build_route_stack(&settings);

    let mut req = Request::get("https://test-publisher.com/__ts/page-bids?path=/2024/article/");
    req.set_header("sec-fetch-site", "same-origin");
    let services = test_runtime_services(&req);

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route page-bids request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::OK,
        "should serve the page-bids response"
    );
    assert_eq!(
        resp.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "should keep the per-user cache directive despite operator Cache-Control"
    );
    assert_eq!(
        resp.get_header_str("surrogate-control"),
        None,
        "should not let operator headers re-enable shared surrogate caching"
    );
    assert_eq!(
        resp.get_header_str("fastly-surrogate-control"),
        None,
        "should not let operator headers re-enable Fastly surrogate caching"
    );
}

#[test]
fn page_bids_cross_site_request_is_rejected_at_the_route() {
    let base = base_route_settings_toml();
    let prebid = prebid_integration_toml();
    let config = format!(
        r#"{base}

{prebid}

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000

            [creative_opportunities]
            gam_network_id = "1234"
        "#,
    );
    let settings =
        Settings::from_toml(&config).expect("should parse page-bids route test settings");
    let (orchestrator, integration_registry) = build_route_stack(&settings);

    let mut req = Request::get("https://test-publisher.com/__ts/page-bids?path=/2024/article/");
    req.set_header("sec-fetch-site", "cross-site");
    let services = test_runtime_services(&req);

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route cross-site page-bids request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::FORBIDDEN,
        "should reject cross-site page-bids requests"
    );
}

#[test]
fn page_bids_options_preflight_is_rejected_at_the_route() {
    // OPTIONS must not fall through to the publisher origin (which may return
    // permissive CORS); the GET handler's legacy `X-TSJS-Page-Bids` fallback
    // relies on this endpoint never granting a preflight.
    let base = base_route_settings_toml();
    let prebid = prebid_integration_toml();
    let config = format!(
        r#"{base}

{prebid}

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000

            [creative_opportunities]
            gam_network_id = "1234"
        "#,
    );
    let settings =
        Settings::from_toml(&config).expect("should parse page-bids route test settings");
    let (orchestrator, integration_registry) = build_route_stack(&settings);

    let req = Request::new(
        Method::OPTIONS,
        "https://test-publisher.com/__ts/page-bids?path=/2024/article/",
    );
    let services = test_runtime_services(&req);

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route page-bids preflight request",
    );

    assert_eq!(
        resp.get_status(),
        StatusCode::FORBIDDEN,
        "should reject the page-bids CORS preflight at the adapter"
    );
    assert_eq!(
        resp.get_header_str(header::CACHE_CONTROL),
        Some("private, no-store"),
        "preflight rejection must not be shared-cached"
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

    let resp = route_buffered_response(
        &settings,
        &orchestrator,
        &integration_registry,
        &services,
        req,
        "should route S3 asset request",
    );

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
    settings.publisher.origin_url = "https://".to_string();
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
        StatusCode::BAD_GATEWAY,
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
