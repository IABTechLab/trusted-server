//! An integration that lives outside `trusted-server-core` and exercises every
//! part of the integration seam from a vendor crate's position.
//!
//! One registration carries a browser module, a proxy route, a geo provider,
//! an Edge Cookie identity provider and a device provider, alongside its own
//! configuration block. The builder adds a request preparer, and the crate
//! also supplies an auction provider builder. The round-trip tests in
//! `crates/trusted-server-adapter-axum/tests/seam_probe.rs` drive each of
//! those through a real adapter, so the seam is proven by a caller that core
//! does not know about.
//!
//! This crate is a test fixture and must never ship in a deployment. It
//! reports internal request state over an unauthenticated route, serves a
//! browser module that does nothing useful, and resolves location from static
//! configuration rather than from the request. It is a development dependency
//! of the Axum adapter only, so no adapter's production build reaches it.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::header::{self, HeaderValue};
use http::{Request, Response};
use serde::Deserialize;
use serde_json::json;
use trusted_server_core::auction::AuctionProviderBuilder;
use trusted_server_core::auction::provider::{AuctionProvider, ProviderRequestOutcome};
use trusted_server_core::auction::types::{AuctionContext, AuctionRequest, AuctionResponse};
use trusted_server_core::ec::device::{DeviceProvider, DeviceSignals};
use trusted_server_core::ec::provider::{
    EdgeCookieProvider, GeneratedEdgeCookie, IdentityInput, ProviderCode,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::integrations::{
    CarriedJsModule, IntegrationBuilder, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration,
};
use trusted_server_core::platform::{
    GeoInfo, PlatformError, PlatformGeo, PlatformResponse, RuntimeServices,
};
use trusted_server_core::settings::{IntegrationConfig, Settings};
use validator::Validate;

/// Integration id, which is also the key of the probe's configuration block
/// (`[integrations.seam_probe]`) and the value `[geo] provider` names to
/// select the probe's geo provider.
pub const SEAM_PROBE_ID: &str = "seam_probe";

/// Source label the registry and the orchestrator use in duplicate-id and
/// duplicate-name errors.
pub const SEAM_PROBE_SOURCE: &str = "trusted-server-integration-seam-probe";

/// Auction provider name this crate declares, which `[auction] providers` may
/// name.
pub const SEAM_PROBE_AUCTION_PROVIDER: &str = "seam_probe";

/// Path of the proxy route that reports what the seam delivered.
///
/// The route itself is built through [`IntegrationProxy::get`], so
/// `report_route_is_the_documented_path` pins this constant to the path the
/// registry actually routes.
pub const SEAM_PROBE_REPORT_PATH: &str = "/integrations/seam_probe/report";

/// Path suffix of the report route, relative to the proxy prefix.
const REPORT_ROUTE_SUFFIX: &str = "/report";

/// The browser module this registration carries, built outside
/// `trusted-server-js`.
pub const PROBE_JS: &str = include_str!("../js/probe.js");

/// SHA-256 of [`PROBE_JS`], hex encoded, lower case.
///
/// The registry rejects a registration whose declared hash does not match its
/// source, so a stale literal is a startup error rather than a stale script.
/// `carried_module_hash_literal_matches_its_source` keeps this honest.
pub const PROBE_JS_SHA256: &str =
    "4711826cac0ce3df585c4a2b30f6ad5d565900a81e89f0ee00148efff2342079";

/// Timeout the probe's auction provider reports. It answers immediately, so
/// no test turns on the value.
const SEAM_PROBE_AUCTION_TIMEOUT_MS: u32 = 2000;

/// Message the probe's own deploy rule rejects a bad country with, so a test
/// can prove deploy validation reached a vendor's rules rather than stopping
/// at the core ones.
pub const SEAM_PROBE_COUNTRY_MESSAGE: &str =
    "`[integrations.seam_probe] country` must be exactly two letters";

/// Request header a caller sets to have the preparer count that request.
///
/// The header's value is a token the caller chooses, so tests running in
/// parallel count into separate entries.
pub const SEAM_PROBE_COUNT_HEADER: &str = "x-seam-probe-count";

/// How many times the preparer ran for each counting token.
///
/// Request extensions die with the request, so a route that reports nothing
/// about them cannot say how many times its request was prepared. This
/// counter lets a caller pin the once-per-request invariant on any route,
/// including the named routes that return a fixed response.
static PREPARE_COUNTS: LazyLock<Mutex<BTreeMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// How many times the preparer ran for requests carrying `token` in
/// [`SEAM_PROBE_COUNT_HEADER`].
///
/// # Panics
///
/// Panics when the counter's lock is poisoned.
#[must_use]
pub fn prepare_runs_for(token: &str) -> usize {
    PREPARE_COUNTS
        .lock()
        .expect("should lock the seam probe prepare counts")
        .get(token)
        .copied()
        .unwrap_or(0)
}

/// Records one preparer run against the token the request carries, if any.
fn count_prepare_run(request: &Request<EdgeBody>) {
    let Some(token) = request
        .headers()
        .get(SEAM_PROBE_COUNT_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let mut counts = PREPARE_COUNTS
        .lock()
        .expect("should lock the seam probe prepare counts");
    *counts.entry(token.to_string()).or_insert(0) += 1;
}

/// Marker the request preparer inserts into the request extensions.
///
/// The proxy route reports [`Self::runs`], so a test observes both that the
/// preparer ran and how many times the request path ran it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeamProbePrepared {
    /// How many times the preparer ran for this request.
    pub runs: usize,
}

/// The probe's own configuration block, `[integrations.seam_probe]`.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SeamProbeConfig {
    /// Whether the probe is enabled. Defaults to enabled, so writing the
    /// block is enough to switch the probe on.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Country the probe's geo provider resolves for every request.
    ///
    /// Rejected by [`validate`] when it is not two letters, which is the
    /// vendor-owned rule the deploy validation round trip exercises.
    #[serde(default = "default_country")]
    pub country: String,
    /// Whether the registration declares the probe's geo provider.
    ///
    /// Set this to `false` to build an enabled module that supplies no geo
    /// provider, which is what `[geo] provider = "seam_probe"` must reject at
    /// startup.
    #[serde(default = "default_declares_geo")]
    pub declares_geo: bool,
}

impl IntegrationConfig for SeamProbeConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

const fn default_enabled() -> bool {
    true
}

/// `ZZ` is reserved in ISO 3166-1 for private use, so it names no real place.
fn default_country() -> String {
    "ZZ".to_string()
}

const fn default_declares_geo() -> bool {
    true
}

/// Geo provider that resolves the country the probe's configuration names,
/// whatever the client address is.
pub struct SeamProbeGeo {
    country: String,
}

impl SeamProbeGeo {
    /// Creates a provider that resolves `country` for every request.
    #[must_use]
    pub const fn new(country: String) -> Self {
        Self { country }
    }
}

/// The probe's Edge Cookie provider, declared on its registration so identity
/// reaches core the same way location does.
///
/// It derives a value from the request evidence rather than a constant, so a
/// test can tell the difference between the module's provider running and
/// core's built-in one running.
#[derive(Debug)]
pub struct SeamProbeEc;

/// The four-character code core stamps on identifiers this provider owns.
const SEAM_PROBE_EC_CODE: ProviderCode = trusted_server_core::provider_code!("sprb");

#[async_trait::async_trait(?Send)]
impl EdgeCookieProvider for SeamProbeEc {
    fn id(&self) -> &'static str {
        SEAM_PROBE_ID
    }

    fn code(&self) -> ProviderCode {
        SEAM_PROBE_EC_CODE
    }

    async fn generate(
        &self,
        request_info: &dyn RequestInfo,
        _input: &IdentityInput<'_>,
        _services: &trusted_server_core::platform::RuntimeServices,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        // Derived from evidence so the test can prove this ran rather than the
        // built-in provider, and prove the evidence actually arrived.
        let ip = request_info.client_ip();
        let id = format!("seam-probe-{}", if ip.is_empty() { "no-ip" } else { ip });
        Ok(GeneratedEdgeCookie {
            id: Some(id),
            response_headers: Vec::new(),
        })
    }

    fn accepts_id(&self, value: &str) -> bool {
        value.starts_with("seam-probe-")
    }
}

/// The probe's device provider, declared on the same registration.
#[derive(Debug)]
pub struct SeamProbeDevice;

#[async_trait::async_trait(?Send)]
impl DeviceProvider for SeamProbeDevice {
    fn id(&self) -> &'static str {
        SEAM_PROBE_ID
    }

    async fn detect(
        &self,
        _request_info: &dyn RequestInfo,
        _services: &trusted_server_core::platform::RuntimeServices,
    ) -> DeviceSignals {
        DeviceSignals {
            is_mobile: 1,
            platform_class: Some("seam-probe".to_owned()),
            known_browser: Some(true),
            looks_like_browser: true,
            ja4_class: None,
            h2_fp_hash: None,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformGeo for SeamProbeGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &trusted_server_core::platform::RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo {
            city: "Example City".to_string(),
            country: self.country.clone(),
            continent: "Example Continent".to_string(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        }))
    }
}

/// Proxy that reports what the seam delivered to this request.
pub struct SeamProbeProxy;

#[async_trait(?Send)]
impl IntegrationProxy for SeamProbeProxy {
    fn integration_name(&self) -> &'static str {
        SEAM_PROBE_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get(REPORT_ROUTE_SUFFIX)]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let country = match services
            .geo()
            .lookup(services.client_info().client_ip, services)
            .await
        {
            Ok(geo) => geo.map(|info| info.country),
            Err(error) => {
                log::warn!("seam probe geo lookup failed: {error}");
                None
            }
        };
        let prepared = req.extensions().get::<SeamProbePrepared>().copied();

        let body = json!({
            "module": SEAM_PROBE_ID,
            "geo_country": country,
            "request_preparer_runs": prepared.map_or(0, |marker| marker.runs),
        })
        .to_string();

        let mut response = Response::new(EdgeBody::from(body));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(response)
    }
}

/// Auction provider the probe contributes, which answers every bid request
/// immediately with no bids.
///
/// It exists so a name declared outside core can satisfy
/// `[auction] providers`, not to bid.
pub struct SeamProbeAuctionProvider;

#[async_trait(?Send)]
impl AuctionProvider for SeamProbeAuctionProvider {
    fn provider_name(&self) -> &'static str {
        SEAM_PROBE_AUCTION_PROVIDER
    }

    async fn request_bids(
        &self,
        _request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
            SEAM_PROBE_AUCTION_PROVIDER,
            0,
        )))
    }

    async fn parse_response(
        &self,
        _response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        Ok(AuctionResponse::no_bid(
            SEAM_PROBE_AUCTION_PROVIDER,
            response_time_ms,
        ))
    }

    fn timeout_ms(&self) -> u32 {
        SEAM_PROBE_AUCTION_TIMEOUT_MS
    }
}

/// Rejects a country that is not exactly two ASCII letters.
///
/// # Errors
///
/// Returns [`TrustedServerError::Integration`] carrying
/// [`SEAM_PROBE_COUNTRY_MESSAGE`] and the offending value.
fn check_country(country: &str) -> Result<(), Report<TrustedServerError>> {
    if country.len() == 2 && country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Ok(());
    }

    Err(Report::new(TrustedServerError::Integration {
        integration: SEAM_PROBE_ID.to_string(),
        message: format!("{SEAM_PROBE_COUNTRY_MESSAGE}, not `{country}`"),
    }))
}

/// Reads the probe's configuration block, or `None` when the probe is not
/// enabled.
///
/// # Errors
///
/// Returns an error when the block is present but cannot be parsed or fails
/// validation.
fn read_config(settings: &Settings) -> Result<Option<SeamProbeConfig>, Report<TrustedServerError>> {
    settings.integration_config::<SeamProbeConfig>(SEAM_PROBE_ID)
}

/// Builds the probe's registration, or `None` when the probe is not enabled.
///
/// # Errors
///
/// Returns an error when the configuration block cannot be parsed or fails
/// validation, or when the probe is enabled with a country that is not two
/// letters.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = read_config(settings)? else {
        return Ok(None);
    };
    check_country(&config.country)?;

    let mut registration = IntegrationRegistration::builder(SEAM_PROBE_ID)
        .with_proxy(Arc::new(SeamProbeProxy))
        .with_js_module(CarriedJsModule {
            source: PROBE_JS,
            sha256: PROBE_JS_SHA256,
        });
    if config.declares_geo {
        registration = registration.with_geo_provider(Arc::new(SeamProbeGeo::new(config.country)));
    }
    {
        // Identity and device are declared the same way location is, so one
        // module supplies all three and there is no second mechanism.
        registration = registration
            .with_ec_provider(Arc::new(SeamProbeEc))
            .with_device_provider(Arc::new(SeamProbeDevice));
    }

    Ok(Some(registration.build()))
}

/// Validates the probe's configuration for deployment and reports whether the
/// probe is enabled.
///
/// # Errors
///
/// Returns an error when the configuration block cannot be parsed or fails
/// validation, or when the probe is enabled with a country that is not two
/// letters.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = read_config(settings)? else {
        return Ok(false);
    };
    check_country(&config.country)?;
    Ok(true)
}

/// Inserts [`SeamProbePrepared`] into the request extensions, counting how
/// many times the request path ran it.
///
/// Runs whether or not the probe is enabled, which is the contract the
/// registry gives preparers.
///
/// # Errors
///
/// Never returns an error; the result type matches
/// `IntegrationPrepareRequestFn`.
pub fn prepare_request(
    _settings: &Settings,
    request: &mut Request<EdgeBody>,
) -> Result<(), Report<TrustedServerError>> {
    count_prepare_run(request);
    let runs = request
        .extensions()
        .get::<SeamProbePrepared>()
        .map_or(0, |marker| marker.runs);
    request
        .extensions_mut()
        .insert(SeamProbePrepared { runs: runs + 1 });
    Ok(())
}

/// Builds the probe's auction providers, empty when the probe is not enabled.
///
/// # Errors
///
/// Returns an error when the probe's configuration block cannot be parsed.
pub fn register_auction_providers(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    match read_config(settings)? {
        Some(_) => Ok(vec![
            Arc::new(SeamProbeAuctionProvider) as Arc<dyn AuctionProvider>
        ]),
        None => Ok(Vec::new()),
    }
}

/// The probe's integration builder, for an adapter's
/// `routes_with_registrations` or `build_state_with_registrations`.
///
/// # Examples
///
/// ```
/// use trusted_server_integration_seam_probe::{builder, SEAM_PROBE_ID};
///
/// assert_eq!(builder().id(), SEAM_PROBE_ID);
/// ```
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(SEAM_PROBE_ID, SEAM_PROBE_SOURCE, register, validate)
        .with_request_preparer(prepare_request)
}

/// The probe's auction provider builder, for an adapter's
/// `routes_with_registrations` or `build_state_with_registrations`.
///
/// # Examples
///
/// ```
/// use trusted_server_integration_seam_probe::{auction_builder, SEAM_PROBE_AUCTION_PROVIDER};
///
/// assert_eq!(auction_builder().name(), SEAM_PROBE_AUCTION_PROVIDER);
/// ```
#[must_use]
pub fn auction_builder() -> AuctionProviderBuilder {
    AuctionProviderBuilder::new(
        SEAM_PROBE_AUCTION_PROVIDER,
        SEAM_PROBE_SOURCE,
        register_auction_providers,
        validate,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use sha2::{Digest as _, Sha256};
    use trusted_server_core::platform::{
        ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
        PlatformSecretStore, StoreId, StoreName, UnavailableHttpClient, UnavailableKvStore,
    };

    /// Config store that answers nothing, so a test can build
    /// [`RuntimeServices`] without a real platform.
    ///
    /// [`SeamProbeGeo::lookup`] resolves from the country it holds and reads
    /// none of the services, so the store is never queried.
    struct StubConfigStore;

    impl PlatformConfigStore for StubConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
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

    /// Secret store that answers nothing, paired with [`StubConfigStore`].
    struct StubSecretStore;

    impl PlatformSecretStore for StubSecretStore {
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

    /// Backend manager that registers nothing, paired with the stub stores.
    struct StubBackend;

    impl PlatformBackend for StubBackend {
        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    /// Builds a minimal [`RuntimeServices`] the geo lookup can be handed. Every
    /// service is a stub or a core-provided unavailable implementation because
    /// [`SeamProbeGeo::lookup`] reads none of them.
    fn build_probe_services() -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(StubConfigStore))
            .secret_store(Arc::new(StubSecretStore))
            .kv_store(Arc::new(UnavailableKvStore))
            .backend(Arc::new(StubBackend))
            .http_client(Arc::new(UnavailableHttpClient))
            .geo(Arc::new(DisabledGeo))
            .client_info(ClientInfo::default())
            .build()
    }

    /// Settings carrying a `[integrations.seam_probe]` block with `body`
    /// appended to it.
    fn settings_with_probe(body: &str) -> Settings {
        Settings::from_toml(&format!(
            r#"
                [[handlers]]
                path = "^/_ts/admin"
                username = "admin"
                password = "admin-pass"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                origin_url = "https://origin.test-publisher.example.com"
                proxy_secret = "seam-probe-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                default_country = "US"
                assume_single_jurisdiction = true

                [integrations.seam_probe]
                {body}
            "#
        ))
        .expect("should parse seam probe test settings")
    }

    /// The registry rejects a carried module whose declared hash is wrong, so
    /// this keeps the fixture's literal honest rather than letting every other
    /// test fail with a startup error.
    #[test]
    fn carried_module_hash_literal_matches_its_source() {
        assert_eq!(
            hex::encode(Sha256::digest(PROBE_JS.as_bytes())),
            PROBE_JS_SHA256,
            "PROBE_JS_SHA256 should be the SHA-256 of js/probe.js; if the source is unchanged, check that the file was checked out with LF line endings as .gitattributes requires"
        );
    }

    #[test]
    fn report_route_is_the_documented_path() {
        let routes = SeamProbeProxy.routes();

        assert_eq!(routes.len(), 1, "should register exactly one route");
        assert_eq!(
            routes[0].path, SEAM_PROBE_REPORT_PATH,
            "should route the documented report path"
        );
    }

    #[test]
    fn validate_accepts_a_two_letter_country() {
        let settings = settings_with_probe("country = \"ZZ\"");

        assert!(
            validate(&settings).expect("should accept a two letter country"),
            "should report the probe as enabled"
        );
    }

    #[test]
    fn validate_rejects_a_country_that_is_not_two_letters() {
        let settings = settings_with_probe("country = \"ZZZ\"");

        let error = validate(&settings).expect_err("should reject a three letter country");

        assert!(
            error.to_string().contains(SEAM_PROBE_COUNTRY_MESSAGE),
            "should reject with the probe's own message: {error}"
        );
    }

    #[test]
    fn validate_reports_not_enabled_without_a_configuration_block() {
        let settings = Settings::from_toml(
            r#"
                [[handlers]]
                path = "^/_ts/admin"
                username = "admin"
                password = "admin-pass"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                origin_url = "https://origin.test-publisher.example.com"
                proxy_secret = "seam-probe-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                default_country = "US"
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse settings without a probe block");

        assert!(
            !validate(&settings).expect("should validate settings without a probe block"),
            "should report the probe as not enabled"
        );
        assert!(
            register(&settings)
                .expect("should build no registration")
                .is_none(),
            "should build no registration when the probe is not configured"
        );
    }

    #[test]
    fn registration_declares_no_geo_provider_when_the_block_turns_it_off() {
        let settings = settings_with_probe("country = \"ZZ\"\ndeclares_geo = false");

        let registration = register(&settings)
            .expect("should build a registration")
            .expect("should be enabled");

        assert!(
            registration.geo_provider.is_none(),
            "should declare no geo provider when declares_geo is false"
        );
    }

    #[tokio::test]
    async fn geo_provider_resolves_the_configured_country() {
        let provider = SeamProbeGeo::new("ZZ".to_string());
        let services = build_probe_services();

        let geo = provider
            .lookup(None, &services)
            .await
            .expect("should resolve location")
            .expect("should return geo information");

        assert_eq!(geo.country, "ZZ", "should resolve the configured country");
    }

    #[test]
    fn prepare_request_counts_each_run() {
        let settings = settings_with_probe("country = \"ZZ\"");
        let mut request = Request::new(EdgeBody::from(Vec::new()));

        prepare_request(&settings, &mut request).expect("should prepare the request");
        prepare_request(&settings, &mut request).expect("should prepare the request again");

        assert_eq!(
            request.extensions().get::<SeamProbePrepared>(),
            Some(&SeamProbePrepared { runs: 2 }),
            "should count both runs"
        );
    }
}
