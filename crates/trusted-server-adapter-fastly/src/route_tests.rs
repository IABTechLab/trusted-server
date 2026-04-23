use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::http::StatusCode;
use fastly::{mime, Request};
use trusted_server_core::auction::build_orchestrator;
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

#[test]
fn configured_missing_consent_store_only_breaks_consent_routes() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let discovery_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services(&discovery_req);
    let discovery_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &discovery_services,
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
fn ja4_debug_route_returns_plain_text_fallback_response() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");

    let req = Request::get("https://test.com/_ts/debug/ja4");
    let runtime_services = test_runtime_services(&req);
    let mut response = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        req,
    ))
    .expect("should route ja4 debug request");

    assert_eq!(
        response.get_status(),
        StatusCode::OK,
        "should return 200 OK for the ja4 debug route"
    );
    assert_eq!(
        response.get_content_type(),
        Some(mime::TEXT_PLAIN_UTF_8),
        "should return plain text content for the ja4 debug route"
    );
    assert_eq!(
        response.get_header_str("cache-control"),
        Some("no-store, private"),
        "should disable caching for the ja4 debug route"
    );

    let body = response.take_body_str();

    assert!(
        body.contains("ja4:         unavailable"),
        "should include the JA4 fallback when Fastly omits the fingerprint"
    );
    assert!(
        body.contains("h2_fp:       unavailable"),
        "should include the H2 fingerprint fallback when Fastly omits it"
    );
    assert!(
        body.contains("cipher:      unavailable"),
        "should include the cipher fallback when Fastly omits it"
    );
    assert!(
        body.contains("tls_version: unavailable"),
        "should include the TLS version fallback when Fastly omits it"
    );
    assert!(
        body.contains("user-agent:  none"),
        "should include the user-agent fallback when the header is absent"
    );
    assert!(
        body.contains("ch-mobile:   not sent"),
        "should include the mobile client hints fallback when the header is absent"
    );
    assert!(
        body.contains("ch-platform: not sent"),
        "should include the platform client hints fallback when the header is absent"
    );
}
