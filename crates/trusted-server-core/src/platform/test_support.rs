use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose};
use ed25519_dalek::SigningKey;
use error_stack::Report;
use rand::rngs::OsRng;

use super::{
    ClientInfo, GeoInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
    RuntimeServices, StoreId, StoreName,
};
use crate::request_signing::{JWKS_STORE_NAME, SIGNING_STORE_NAME};
use crate::test_support::scripted_client::{ScriptedHttpClient, TestClock};

pub(crate) struct NoopConfigStore;

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _store_name: &StoreName, _key: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct NoopSecretStore;

#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct HashMapConfigStore {
    data: HashMap<String, String>,
}

impl HashMapConfigStore {
    pub(crate) fn new(data: HashMap<String, String>) -> Self {
        Self { data }
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for HashMapConfigStore {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.data
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::ConfigStore))
    }

    async fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct HashMapSecretStore {
    data: HashMap<String, Vec<u8>>,
}

impl HashMapSecretStore {
    pub(crate) fn new(data: HashMap<String, Vec<u8>>) -> Self {
        Self { data }
    }
}

#[async_trait::async_trait(?Send)]
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

    async fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

/// Build a [`RuntimeServices`] instance with a custom config store and a custom secret store.
///
/// Use this when a test exercises code that reads from config AND secret stores,
/// such as `request_signing::signing` and `request_signing::rotation`.
pub(crate) fn build_services_with_config_and_secret(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo::default())
        .build()
}

pub(crate) fn build_services_with_config_and_secret_and_client_ip(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
    client_ip: IpAddr,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: Some(client_ip),
            ..ClientInfo::default()
        })
        .build()
}

pub(crate) fn build_request_signing_services() -> RuntimeServices {
    let signing_key = SigningKey::generate(&mut OsRng);
    let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());
    let x_b64 = general_purpose::URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    let jwk_json =
        format!(r#"{{"kty":"OKP","crv":"Ed25519","x":"{x_b64}","kid":"test-kid","alg":"EdDSA"}}"#);

    let mut config_data = HashMap::new();
    config_data.insert("current-kid".to_owned(), "test-kid".to_owned());
    config_data.insert("test-kid".to_owned(), jwk_json);

    let mut secret_data = HashMap::new();
    secret_data.insert("test-kid".to_owned(), key_b64.into_bytes());

    build_services_with_config_and_secret(
        HashMapConfigStore::new(config_data),
        HashMapSecretStore::new(secret_data),
    )
}

pub(crate) fn build_services_with_config(
    config_store: impl PlatformConfigStore + 'static,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo::default())
        .build()
}

pub(crate) fn noop_services() -> RuntimeServices {
    build_services_with_config(NoopConfigStore)
}

/// Build test services whose `EdgeZero` outbound requests go to `client`.
///
/// The services clock reads `clock`, so a test that scripts time shares one
/// clock between the scripted replies and the code under test. Every other
/// service is a no-op.
pub(crate) fn build_services_with_outbound_client(
    client: &ScriptedHttpClient,
    clock: &TestClock,
) -> RuntimeServices {
    RuntimeServices {
        outbound_client: client.http_client(),
        clock: clock.monotonic_clock(),
        ..noop_services()
    }
}

/// Build a [`RuntimeServices`] whose auction telemetry sink is the supplied
/// recording (or otherwise custom) sink, so tests can assert which terminal
/// auction events were emitted.
pub(crate) fn noop_services_with_telemetry_sink(
    auction_telemetry_sink: Arc<dyn crate::auction::telemetry::AuctionTelemetrySink>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .auction_telemetry_sink(auction_telemetry_sink)
        .client_info(ClientInfo::default())
        .build()
}

pub(crate) fn noop_services_with_client_ip(ip: IpAddr) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: Some(ip),
            ..ClientInfo::default()
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_services_with_config_and_secret_uses_provided_stores() {
        // Arrange: noop stores
        let services = build_services_with_config_and_secret(NoopConfigStore, NoopSecretStore);

        // Act: both stores return Unsupported (confirming the injected impls are active)
        let config_result = services.config_store().get(&StoreName::from("s"), "k");
        let secret_result = services
            .secret_store()
            .get_bytes(&StoreName::from("s"), "k");

        assert!(
            config_result.is_err(),
            "should delegate to injected config store"
        );
        assert!(
            secret_result.is_err(),
            "should delegate to injected secret store"
        );
    }

    #[test]
    fn hash_map_stores_return_preset_values() {
        let mut config = HashMap::new();
        config.insert("current-kid".to_owned(), "test-kid".to_owned());

        let mut secrets = HashMap::new();
        secrets.insert("test-kid".to_owned(), b"secret-material".to_vec());

        let services = build_services_with_config_and_secret(
            HashMapConfigStore::new(config),
            HashMapSecretStore::new(secrets),
        );

        assert_eq!(
            services
                .config_store()
                .get(&JWKS_STORE_NAME, "current-kid")
                .expect("should read current-kid from config test store"),
            "test-kid"
        );
        assert_eq!(
            services
                .secret_store()
                .get_bytes(&SIGNING_STORE_NAME, "test-kid")
                .expect("should read signing key bytes from secret test store"),
            b"secret-material".to_vec()
        );
    }

    #[test]
    fn build_request_signing_services_provides_current_kid_and_signing_key() {
        let services = build_request_signing_services();

        let kid = services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .expect("should expose current-kid in config store");
        let key_bytes = services
            .secret_store()
            .get_bytes(&SIGNING_STORE_NAME, &kid)
            .expect("should expose signing key bytes in secret store");

        assert_eq!(kid, "test-kid", "should use the standard signing test kid");
        assert!(
            !key_bytes.is_empty(),
            "should provide key material for the current signing key"
        );
    }
}
