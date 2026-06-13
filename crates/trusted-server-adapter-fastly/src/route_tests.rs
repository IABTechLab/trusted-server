use std::net::IpAddr;
use std::sync::Arc;

use crate::compat;
use edgezero_core::key_value_store::NoopKvStore;
use error_stack::Report;
use fastly::http::StatusCode;
use fastly::Request;
use trusted_server_core::auction::build_orchestrator;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::registry::PartnerRegistry;
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
            tls_protocol: req.get_tls_protocol().ok().flatten().map(str::to_string),
            tls_cipher: req
                .get_tls_cipher_openssl_name()
                .ok()
                .flatten()
                .map(str::to_string),
        })
        .build()
}

#[test]
fn routes_use_request_local_consent() {
    let settings = create_test_settings();
    let orchestrator = build_orchestrator(&settings).expect("should build auction orchestrator");
    let integration_registry =
        IntegrationRegistry::new(&settings).expect("should create integration registry");
    let partner_registry = PartnerRegistry::empty();

    let discovery_fastly_req = Request::get("https://test.com/.well-known/trusted-server.json");
    let discovery_services = test_runtime_services(&discovery_fastly_req);
    let discovery_resp = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &discovery_services,
        compat::from_fastly_request(discovery_fastly_req),
        DeviceSignals::derive("", None, None),
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
        compat::from_fastly_request(admin_fastly_req),
        DeviceSignals::derive("", None, None),
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
