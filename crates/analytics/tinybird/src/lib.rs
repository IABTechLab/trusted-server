//! Tinybird as an analytics module: each auction's telemetry rows sent to the
//! Events API as one request.
//!
//! `[analytics] module = "tinybird"` selects the module and
//! `[analytics.tinybird]` holds its settings. The module supplies the auction
//! telemetry sink core hands each settled auction's rows to. The sink
//! serializes a batch as NDJSON, starts one Events API request for it and
//! drops the pending request without waiting, so the Events API never delays
//! a reader's page.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, header, request_builder};
use error_stack::{Report, ResultExt as _};
use serde::{Deserialize, Serialize};
use validator::Validate;

use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::host_header::validate_host_header_override_value;
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistration, ModuleSecretSetting,
};
use trusted_server_core::platform::{PlatformBackendSpec, PlatformHttpRequest, RuntimeServices};
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::{IntegrationConfig, Settings};

/// The id the module's registration carries.
const TINYBIRD_INTEGRATION_ID: &str = "tinybird";

/// The name `[analytics]` selects the module by, from this crate's folder.
/// Its settings are the table `[analytics.tinybird]`.
pub const MODULE: &str = "analytics.tinybird";

const EVENTS_PATH: &str = "/v0/events";
const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(2);
const BETWEEN_BYTES_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ROWS_PER_AUCTION_BATCH: usize = 512;

/// The settings of `[analytics.tinybird]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TinybirdSettings {
    /// Regional Tinybird API host, without scheme or path.
    #[serde(default)]
    pub api_host: String,
    /// Deprecated feature-specific store selector accepted for migration only.
    #[serde(default, skip_serializing)]
    pub secret_store: Option<String>,
    /// Auction Events API datasource name.
    #[serde(default = "default_auction_dataset")]
    pub auction_dataset: String,
    /// The name of the secret holding the auction datasource's APPEND token,
    /// which holds the token itself once the settings have loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auction_token_secret: Option<Redacted<String>>,
    /// Reserved for future access-log telemetry.
    ///
    /// `true` is refused until an access-log emitter exists, so an operator
    /// cannot enable a setting that silently emits nothing.
    #[serde(default)]
    pub access_enabled: bool,
    /// Future access-log Events API datasource name.
    #[serde(default = "default_access_dataset")]
    pub access_dataset: String,
    /// Deprecated placeholder for the unwired access-log APPEND token.
    #[serde(default, skip_serializing)]
    pub access_token_secret: Option<Redacted<String>>,
    /// Future fraction of requests to emit for optional access telemetry.
    #[serde(default)]
    pub access_sample_rate: f64,
    /// Defensive maximum NDJSON body size for one Events API request.
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
}

fn default_auction_dataset() -> String {
    "auction_events_raw".to_owned()
}

fn default_access_dataset() -> String {
    "access_logs_raw".to_owned()
}

fn default_max_body_bytes() -> usize {
    1024 * 1024
}

impl Default for TinybirdSettings {
    fn default() -> Self {
        Self {
            api_host: String::new(),
            secret_store: None,
            auction_dataset: default_auction_dataset(),
            auction_token_secret: None,
            access_enabled: false,
            access_dataset: default_access_dataset(),
            access_token_secret: None,
            access_sample_rate: 0.0,
            max_body_bytes: default_max_body_bytes(),
        }
    }
}

// The checks need the table trimmed first, which a borrowed `validate` cannot
// do, so they run in `checked` and this reads the table as written.
impl Validate for TinybirdSettings {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        Ok(())
    }
}

impl IntegrationConfig for TinybirdSettings {}

impl TinybirdSettings {
    /// The table trimmed, with the host lower cased and the deprecated
    /// fields dropped, then checked.
    ///
    /// # Errors
    ///
    /// Naming the setting that is missing or malformed.
    fn checked(mut self) -> Result<Self, Report<TrustedServerError>> {
        self.api_host = self.api_host.trim().to_ascii_lowercase();
        if self.secret_store.take().is_some() {
            log::warn!(
                "{MODULE}.secret_store is deprecated and ignored; the token resolves through \
                 the default app-config secret store"
            );
        }
        self.auction_dataset = self.auction_dataset.trim().to_owned();
        self.auction_token_secret = self.auction_token_secret.take().and_then(|value| {
            let value = value.expose().trim().to_owned();
            (!value.is_empty()).then(|| Redacted::new(value))
        });
        self.access_dataset = self.access_dataset.trim().to_owned();
        self.access_token_secret = None;
        self.check()?;
        Ok(self)
    }

    fn check(&self) -> Result<(), Report<TrustedServerError>> {
        if !(0.0..=1.0).contains(&self.access_sample_rate) {
            return Err(configuration(format!(
                "{MODULE}.access_sample_rate must be between 0.0 and 1.0"
            )));
        }
        if self.max_body_bytes < 1024 {
            return Err(configuration(format!(
                "{MODULE}.max_body_bytes must be at least 1024"
            )));
        }
        if self.access_enabled {
            return Err(configuration(format!(
                "{MODULE}.access_enabled is reserved for future access-log telemetry; no \
                 emitter is currently wired"
            )));
        }
        validate_api_host(&self.api_host)?;
        validate_dataset(&self.auction_dataset, &format!("{MODULE}.auction_dataset"))?;
        let token = self.auction_token_secret.as_ref().ok_or_else(|| {
            configuration(format!(
                "{MODULE}.auction_token_secret is required when [analytics] selects the module"
            ))
        })?;
        if token.expose().chars().any(char::is_control) {
            return Err(configuration(format!(
                "{MODULE}.auction_token_secret must be non-empty after secret resolution"
            )));
        }
        Ok(())
    }
}

fn configuration(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

fn validate_api_host(host: &str) -> Result<(), Report<TrustedServerError>> {
    if host.is_empty()
        || host.contains('/')
        || host.contains(':')
        || host.chars().any(char::is_control)
        || host.starts_with("http://")
        || host.starts_with("https://")
    {
        return Err(configuration(format!(
            "{MODULE}.api_host must be a regional host without scheme, port, or path"
        )));
    }
    validate_host_header_override_value(host)
        .map_err(|reason| configuration(format!("{MODULE}.api_host {reason}")))
}

fn validate_dataset(value: &str, setting: &str) -> Result<(), Report<TrustedServerError>> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(configuration(format!(
            "{setting} must be a non-empty datasource identifier"
        )));
    }
    Ok(())
}

/// The token is in use whenever the section selects the module.
fn always_in_use(_table: &serde_json::Map<String, serde_json::Value>) -> bool {
    true
}

/// The one setting of the table that names a secret.
const SECRET_SETTINGS: &[ModuleSecretSetting] = &[ModuleSecretSetting {
    path: &["auction_token_secret"],
    in_use: always_in_use,
}];

/// Validates the table and reports whether `[analytics]` selects the module.
///
/// # Errors
///
/// Returns an error when the table cannot be read, carries a key the module
/// does not know, or fails a check.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<TinybirdSettings>(MODULE)? else {
        return Ok(false);
    };
    config.checked()?;
    Ok(true)
}

/// Registers the module's sink when `[analytics]` selects the module.
///
/// # Errors
///
/// Returns an error when the table cannot be read, carries a key the module
/// does not know, or fails a check.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<TinybirdSettings>(MODULE)? else {
        return Ok(None);
    };
    let sink = TinybirdAuctionTelemetrySink::new(config.checked()?)?;
    Ok(Some(
        IntegrationRegistration::builder(TINYBIRD_INTEGRATION_ID)
            // No browser module. The sink is the whole module.
            .without_js()
            .with_analytics_module(MODULE, Arc::new(sink))
            .build(),
    ))
}

/// The builder a deployment installs to offer this module.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        TINYBIRD_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
    .with_secret_settings(SECRET_SETTINGS)
}

/// The sink that sends each batch to the Events API.
#[derive(Debug, Clone)]
pub struct TinybirdAuctionTelemetrySink {
    target: EventsTarget,
}

#[derive(Debug, Clone)]
struct EventsTarget {
    api_host: String,
    dataset: String,
    append_token: Redacted<String>,
    uri: String,
    backend_spec: PlatformBackendSpec,
    max_body_bytes: usize,
}

impl TinybirdAuctionTelemetrySink {
    /// The sink for a checked table.
    ///
    /// # Errors
    ///
    /// When the table carries no token, which [`TinybirdSettings::checked`]
    /// refuses before this is reached.
    fn new(config: TinybirdSettings) -> Result<Self, Report<TrustedServerError>> {
        let append_token = config
            .auction_token_secret
            .ok_or_else(|| configuration(format!("{MODULE}.auction_token_secret is required")))?;
        Ok(Self {
            target: EventsTarget {
                uri: events_uri(&config.api_host, &config.auction_dataset),
                backend_spec: backend_spec(&config.api_host),
                api_host: config.api_host,
                dataset: config.auction_dataset,
                append_token,
                max_body_bytes: config.max_body_bytes,
            },
        })
    }

    fn validate_batch(batch: &AuctionEventBatch) -> Result<(), Report<TrustedServerError>> {
        if batch.row_count() > MAX_ROWS_PER_AUCTION_BATCH {
            return Err(Report::new(TrustedServerError::Proxy {
                message: format!(
                    "auction telemetry batch has {} rows, exceeding {} row limit",
                    batch.row_count(),
                    MAX_ROWS_PER_AUCTION_BATCH
                ),
            }));
        }
        Ok(())
    }

    fn serialize_batch(
        &self,
        batch: &AuctionEventBatch,
    ) -> Result<String, Report<TrustedServerError>> {
        batch.to_ndjson(self.target.max_body_bytes)
    }

    fn ensure_backend(
        &self,
        services: &RuntimeServices,
    ) -> Result<String, Report<TrustedServerError>> {
        services
            .backend()
            .ensure(&self.target.backend_spec)
            .change_context(TrustedServerError::Proxy {
                message: "Tinybird backend registration failed".to_owned(),
            })
    }

    fn authorization_header(token: &str) -> Result<HeaderValue, Report<TrustedServerError>> {
        HeaderValue::from_str(&format!("Bearer {token}")).change_context(
            TrustedServerError::InvalidHeaderValue {
                message: "invalid Tinybird authorization header".to_owned(),
            },
        )
    }

    fn build_events_request(
        &self,
        body: String,
        auth_header: HeaderValue,
    ) -> Result<edgezero_core::http::Request, Report<TrustedServerError>> {
        request_builder()
            .method(Method::POST)
            .uri(self.target.uri.as_str())
            .header(header::AUTHORIZATION, auth_header)
            .header(header::CONTENT_TYPE, NDJSON_CONTENT_TYPE)
            .body(Body::from(body))
            .change_context(TrustedServerError::Proxy {
                message: "failed to build Tinybird Events API request".to_owned(),
            })
    }

    async fn send_fire_and_forget(
        services: &RuntimeServices,
        request: edgezero_core::http::Request,
        backend_name: String,
    ) -> Result<(), Report<TrustedServerError>> {
        let pending = services
            .http_client()
            .send_async(PlatformHttpRequest::new(request, backend_name))
            .await
            .change_context(TrustedServerError::Proxy {
                message: "failed to start Tinybird Events API request".to_owned(),
            })?;
        drop(pending);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl AuctionTelemetrySink for TinybirdAuctionTelemetrySink {
    async fn emit_auction_events(
        &self,
        services: &RuntimeServices,
        batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>> {
        if batch.is_empty() {
            return Ok(());
        }

        Self::validate_batch(&batch)?;
        let body = self.serialize_batch(&batch)?;
        let body_len = body.len();
        let auth_header = Self::authorization_header(self.target.append_token.expose())?;
        let backend_name = self.ensure_backend(services)?;
        let request = self.build_events_request(body, auth_header)?;

        log::info!(
            "sending auction telemetry to Tinybird dataset={} rows={} bytes={} host={} backend={}",
            self.target.dataset,
            batch.row_count(),
            body_len,
            self.target.api_host,
            backend_name
        );

        Self::send_fire_and_forget(services, request, backend_name).await
    }
}

fn backend_spec(api_host: &str) -> PlatformBackendSpec {
    PlatformBackendSpec {
        scheme: "https".to_owned(),
        host: api_host.to_owned(),
        port: None,
        host_header_override: None,
        certificate_check: true,
        first_byte_timeout: FIRST_BYTE_TIMEOUT,
        between_bytes_timeout: BETWEEN_BYTES_TIMEOUT,
        discriminator: None,
    }
}

fn events_uri(api_host: &str, dataset: &str) -> String {
    format!(
        "https://{api_host}{EVENTS_PATH}?name={}",
        urlencoding::encode(dataset)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use edgezero_core::blob_envelope::BlobEnvelope;
    use error_stack::Report;
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionEventRow};
    use trusted_server_core::config::validate_settings_for_deploy_with;
    use trusted_server_core::config_payload::settings_from_config_blob_with;
    use trusted_server_core::integrations::IntegrationRegistry;
    use trusted_server_core::platform::{
        BackendNamingPolicy, ClientInfo, PlatformBackend, PlatformConfigStore, PlatformError,
        PlatformGeo, PlatformHttpClient, PlatformPendingRequest, PlatformResponse,
        PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId, StoreName,
    };
    use trusted_server_core::test_support::template::{
        template_with_resolved_required_secrets, uncomment_block,
    };
    use trusted_server_core::test_support::tests::crate_test_settings_str;

    use super::*;

    const TEST_USER_AGENT: &str =
        "FictionalBrowser/123.4 (FictionalOS 10.2; FictionalDevice) ExampleRenderer/567.8";

    /// The lines of a table that passes every check.
    const TABLE: &str =
        "api_host = \"api.us-east.aws.tinybird.co\"\nauction_token_secret = \"test-auction-token\"";

    /// A document selecting the module, with `table` as its settings.
    fn settings_str_selecting(table: &str) -> String {
        format!(
            "{}\n[analytics]\nmodule = \"tinybird\"\n\n[analytics.tinybird]\n{table}\n",
            crate_test_settings_str()
        )
    }

    fn settings_selecting(table: &str) -> Settings {
        Settings::from_toml(&settings_str_selecting(table)).expect("the document should parse")
    }

    /// The error `validate` reports for a document selecting the module with
    /// `table` as its settings.
    fn refusal_of(table: &str) -> String {
        let err = validate(&settings_selecting(table)).expect_err("the table should be refused");
        format!("{err:?}")
    }

    struct NoopConfigStore;

    impl PlatformConfigStore for NoopConfigStore {
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

    struct MapSecretStore(HashMap<String, Vec<u8>>);

    impl PlatformSecretStore for MapSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.0
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

    #[derive(Default)]
    struct RecordingBackend {
        specs: Mutex<Vec<PlatformBackendSpec>>,
    }

    impl PlatformBackend for RecordingBackend {
        fn naming_policy(&self) -> BackendNamingPolicy {
            BackendNamingPolicy::Fastly
        }

        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok("tinybird-backend".to_owned())
        }

        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.specs
                .lock()
                .expect("should lock backend specs")
                .push(spec.clone());
            Ok("tinybird-backend".to_owned())
        }
    }

    #[derive(Debug)]
    struct RecordedRequest {
        backend_name: String,
        method: String,
        uri: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    #[derive(Default)]
    struct RecordingHttpClient {
        requests: Mutex<Vec<RecordedRequest>>,
        select_calls: Mutex<usize>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for RecordingHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn send_async(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            let backend_name = request.backend_name;
            let (parts, body) = request.request.into_parts();
            let headers = parts
                .headers
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_owned(), value.to_owned()))
                })
                .collect();
            let recorded = RecordedRequest {
                backend_name,
                method: parts.method.to_string(),
                uri: parts.uri.to_string(),
                headers,
                body: body.into_bytes().unwrap_or_default().to_vec(),
            };
            self.requests
                .lock()
                .expect("should lock recorded requests")
                .push(recorded);
            Ok(PlatformPendingRequest::new(()).with_backend_name("tinybird-backend"))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            *self.select_calls.lock().expect("should lock select calls") += 1;
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopGeo;

    #[async_trait::async_trait(?Send)]
    impl PlatformGeo for NoopGeo {
        async fn lookup(
            &self,
            _client_ip: Option<std::net::IpAddr>,
            _services: &RuntimeServices,
        ) -> Result<Option<trusted_server_core::platform::GeoInfo>, Report<PlatformError>> {
            Ok(None)
        }
    }

    fn test_row() -> AuctionEventRow {
        AuctionEventRow {
            event_ts: "2026-06-23 00:00:00.000".to_owned(),
            event_kind: "summary".to_owned(),
            auction_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            auction_source: "auction_api".to_owned(),
            publisher_domain: "test-publisher.example".to_owned(),
            page_path: "/".to_owned(),
            country: "US".to_owned(),
            region: None,
            is_mobile: 0,
            is_known_browser: 1,
            user_agent: Some(TEST_USER_AGENT.to_owned()),
            gdpr_applies: 0,
            consent_present: 0,
            terminal_status: Some("completed".to_owned()),
            terminal_reason: None,
            slot_count: Some(1),
            total_time_ms: Some(1),
            winning_bid_count: Some(0),
            provider: None,
            provider_role: None,
            status: None,
            provider_response_time_ms: None,
            provider_bid_count: None,
            slot_id: None,
            slot_w: None,
            slot_h: None,
            media_type: None,
            seat: None,
            price_cpm: None,
            currency: None,
            is_win: None,
            ad_domain: None,
            ad_id: None,
            origin_cache_shareable: None,
        }
    }

    fn services(
        backend: Arc<RecordingBackend>,
        http_client: Arc<RecordingHttpClient>,
        secrets: HashMap<String, Vec<u8>>,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(MapSecretStore(secrets)))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(backend)
            .http_client(http_client)
            .geo(Arc::new(
                trusted_server_core::platform::PlatformGeoModule::new("noop", Arc::new(NoopGeo)),
            ))
            .client_info(ClientInfo::default())
            .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
            .build()
    }

    fn tinybird_settings() -> TinybirdSettings {
        TinybirdSettings {
            api_host: "api.us-east.aws.tinybird.co".to_owned(),
            secret_store: None,
            auction_dataset: "auction_events_raw".to_owned(),
            auction_token_secret: Some(Redacted::new("append-token".to_owned())),
            access_enabled: false,
            access_dataset: "access_logs_raw".to_owned(),
            access_token_secret: None,
            access_sample_rate: 0.0,
            max_body_bytes: 1024 * 1024,
        }
    }

    fn sink() -> TinybirdAuctionTelemetrySink {
        TinybirdAuctionTelemetrySink::new(tinybird_settings()).expect("the table carries a token")
    }

    #[test]
    fn events_uri_targets_dataset_on_region_host() {
        assert_eq!(
            events_uri("api.us-east.aws.tinybird.co", "auction_events_raw"),
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction_events_raw"
        );
    }

    #[test]
    fn events_uri_urlencodes_dataset_name() {
        assert_eq!(
            events_uri("api.us-east.aws.tinybird.co", "auction events/raw"),
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction%20events%2Fraw"
        );
    }

    #[test]
    fn backend_spec_uses_matching_tls_host() {
        let spec = backend_spec("api.us-east.aws.tinybird.co");
        assert_eq!(spec.scheme, "https");
        assert_eq!(spec.host, "api.us-east.aws.tinybird.co");
        assert_eq!(spec.host_header_override, None);
        assert!(spec.certificate_check, "should verify Tinybird TLS cert");
    }

    #[test]
    fn sink_posts_ndjson_with_resolved_token_and_does_not_wait() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            Arc::clone(&backend),
            Arc::clone(&http_client),
            HashMap::new(),
        );

        futures::executor::block_on(
            sink().emit_auction_events(&services, AuctionEventBatch::new(vec![test_row()])),
        )
        .expect("should start Tinybird request");

        let specs = backend.specs.lock().expect("should lock specs");
        assert_eq!(specs.len(), 1, "should ensure one backend");
        assert_eq!(specs[0].host, "api.us-east.aws.tinybird.co");
        drop(specs);

        let requests = http_client
            .requests
            .lock()
            .expect("should lock recorded requests");
        assert_eq!(requests.len(), 1, "should start one async POST");
        assert_eq!(requests[0].backend_name, "tinybird-backend");
        assert_eq!(requests[0].method, Method::POST.to_string());
        assert_eq!(
            requests[0].uri,
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction_events_raw"
        );
        assert_eq!(
            header_value(&requests[0].headers, header::CONTENT_TYPE.as_str()),
            Some(NDJSON_CONTENT_TYPE)
        );
        assert_eq!(
            header_value(&requests[0].headers, header::AUTHORIZATION.as_str()),
            Some("Bearer append-token")
        );
        let body = std::str::from_utf8(&requests[0].body).expect("should record utf8 ndjson body");
        assert!(body.ends_with('\n'), "should send newline-delimited JSON");
        assert!(
            body.contains(TEST_USER_AGENT),
            "should send the complete user agent to Tinybird"
        );
        assert_eq!(
            *http_client
                .select_calls
                .lock()
                .expect("should lock select calls"),
            0,
            "should not wait for the Tinybird response"
        );
    }

    #[test]
    fn empty_batch_does_not_dispatch() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            Arc::clone(&backend),
            Arc::clone(&http_client),
            HashMap::new(),
        );

        futures::executor::block_on(
            sink().emit_auction_events(&services, AuctionEventBatch::new(Vec::new())),
        )
        .expect("should ignore empty batch");

        assert!(
            backend.specs.lock().expect("should lock specs").is_empty(),
            "should not ensure backend for empty batches"
        );
        assert!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .is_empty(),
            "should not send empty batches"
        );
    }

    #[test]
    fn sink_drops_row_count_oversize_before_sending() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            backend,
            Arc::clone(&http_client),
            HashMap::from([(
                "tinybird_auction_append_token".to_owned(),
                b"append-token".to_vec(),
            )]),
        );
        let rows = vec![test_row(); MAX_ROWS_PER_AUCTION_BATCH + 1];

        let result = futures::executor::block_on(
            sink().emit_auction_events(&services, AuctionEventBatch::new(rows)),
        );

        assert!(
            result.is_err(),
            "best-effort caller will suppress this error"
        );
        assert!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .is_empty(),
            "should not send oversized row batches"
        );
    }

    #[test]
    fn a_table_without_a_token_gives_no_sink() {
        let err = TinybirdAuctionTelemetrySink::new(TinybirdSettings::default())
            .expect_err("no token, no sink");

        assert!(format!("{err:?}").contains("auction_token_secret is required"));
    }

    #[test]
    fn settings_without_an_analytics_section_register_nothing() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without an analytics section");

        assert!(
            register(&settings).expect("nothing to refuse").is_none(),
            "should register nothing"
        );
        assert!(
            !validate(&settings).expect("nothing to refuse"),
            "should report the module unselected"
        );
        let value = serde_json::to_value(&settings).expect("should serialize");
        assert!(
            value.get("analytics").is_none() && value.get("tinybird").is_none(),
            "should carry no analytics key: {value}"
        );
    }

    #[test]
    fn the_module_is_selected_by_its_short_or_full_name() {
        for name in ["tinybird", "analytics.tinybird"] {
            let toml = format!(
                "{}\n[analytics]\nmodule = \"{name}\"\n\n[analytics.tinybird]\n{TABLE}\n",
                crate_test_settings_str()
            );
            let settings =
                Settings::from_toml(&toml).unwrap_or_else(|err| panic!("{name}: {err:?}"));

            let registration = register(&settings)
                .unwrap_or_else(|err| panic!("{name}: {err:?}"))
                .unwrap_or_else(|| panic!("{name}: should register the sink"));

            let (offered, sink) = registration
                .analytics_module
                .expect("the registration should carry the sink");
            assert_eq!(offered, MODULE);
            assert!(sink.is_enabled(), "{name}: the sink should send");
        }
    }

    #[test]
    fn a_selected_module_survives_the_stored_form() {
        let settings = settings_selecting(TABLE);

        let stored = serde_json::to_value(&settings).expect("should serialize");

        assert_eq!(stored["analytics"]["module"], "tinybird");
        assert_eq!(
            stored["analytics"]["tinybird"]["api_host"],
            "api.us-east.aws.tinybird.co"
        );
        assert!(
            stored.get("tinybird").is_none(),
            "should hold no table at the old name"
        );
    }

    #[test]
    fn the_module_needs_a_regional_host() {
        for table in [
            "api_host = \"https://api.example.com/path\"\nauction_token_secret = \"token\"",
            "auction_token_secret = \"token\"",
        ] {
            assert!(
                refusal_of(table).contains("analytics.tinybird.api_host"),
                "should name the host setting: {}",
                refusal_of(table)
            );
        }
    }

    #[test]
    fn the_module_needs_its_token() {
        for table in [
            "api_host = \"api.us-east.aws.tinybird.co\"",
            "api_host = \"api.us-east.aws.tinybird.co\"\nauction_token_secret = \"\"",
            "api_host = \"api.us-east.aws.tinybird.co\"\nauction_token_secret = \"  \"",
        ] {
            assert!(
                refusal_of(table).contains(
                    "analytics.tinybird.auction_token_secret is required when [analytics] \
                     selects the module"
                ),
                "should say the token is required: {}",
                refusal_of(table)
            );
        }
    }

    #[test]
    fn a_selected_module_with_no_table_is_told_what_it_needs() {
        let toml = format!(
            "{}\n[analytics]\nmodule = \"tinybird\"\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("a selection without a table parses");

        let err = validate(&settings).expect_err("an empty table lacks the host");

        assert!(
            format!("{err:?}").contains("analytics.tinybird.api_host"),
            "should name the first setting the module needs: {err:?}"
        );
    }

    #[test]
    fn access_telemetry_cannot_be_enabled() {
        assert!(
            refusal_of(&format!("{TABLE}\naccess_enabled = true"))
                .contains("analytics.tinybird.access_enabled"),
            "should refuse the reserved setting"
        );
    }

    #[test]
    fn the_sample_rate_and_body_limit_are_bounded() {
        assert!(
            refusal_of(&format!("{TABLE}\naccess_sample_rate = 1.5"))
                .contains("analytics.tinybird.access_sample_rate"),
        );
        assert!(
            refusal_of(&format!("{TABLE}\nmax_body_bytes = 512"))
                .contains("analytics.tinybird.max_body_bytes"),
        );
    }

    #[test]
    fn the_table_refuses_a_key_it_does_not_know() {
        let refusal = refusal_of(&format!("{TABLE}\nenabled = true"));

        assert!(
            refusal.contains("[analytics.tinybird]") && refusal.contains("enabled"),
            "should name the table and the key: {refusal}"
        );
    }

    #[test]
    fn the_deprecated_fields_are_accepted_and_dropped() {
        let settings = settings_selecting(&format!(
            "{TABLE}\nsecret_store = \"legacy-store\"\naccess_token_secret = \"legacy-token\""
        ));

        let config = settings
            .module_config::<TinybirdSettings>(MODULE)
            .expect("should read the table")
            .expect("should select the module")
            .checked()
            .expect("the deprecated fields are accepted");

        assert!(config.secret_store.is_none() && config.access_token_secret.is_none());
        let stored = serde_json::to_string(&config).expect("should serialize");
        assert!(
            !stored.contains("legacy-store") && !stored.contains("legacy-token"),
            "should serialize neither deprecated field: {stored}"
        );
    }

    #[test]
    fn a_table_the_section_does_not_select_is_refused_when_the_settings_load() {
        let toml = format!(
            "{}\n[analytics.tinybird]\n{TABLE}\n",
            crate_test_settings_str()
        );

        let err = Settings::from_toml(&toml).expect_err("a table nothing selects is refused");

        assert!(
            format!("{err:?}").contains("[analytics] selects no module"),
            "should name the section and why: {err:?}"
        );
    }

    #[test]
    fn the_old_top_level_table_is_refused() {
        for table in [
            "enabled = true\napi_host = \"api.us-east.aws.tinybird.co\"",
            "enabled = false",
        ] {
            let toml = format!("{}\n[tinybird]\n{table}\n", crate_test_settings_str());

            let err = Settings::from_toml(&toml).expect_err("the table is not read");

            assert!(
                format!("{err:?}").contains("[tinybird]"),
                "should name the table: {err:?}"
            );
        }
    }

    #[test]
    fn a_name_the_build_does_not_have_is_refused_where_the_registry_is_built() {
        let toml = format!(
            "{}\n[analytics]\nmodule = \"another\"\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("the selection parses");

        let err = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .err()
            .expect("a name nothing supplies is refused");

        let message = format!("{err:?}");
        assert!(
            message.contains("[analytics] selects `another`, which no module this deployment")
                && message.contains("The analytics modules it supplies are [tinybird]"),
            "should list what the section could select: {message}"
        );
    }

    #[test]
    fn the_registry_resolves_the_selected_sink() {
        let registry =
            IntegrationRegistry::with_registrations(&settings_selecting(TABLE), &[builder()])
                .expect("should build the registry");

        let sink = registry
            .analytics_module()
            .expect("should resolve the selected sink");

        assert!(sink.is_enabled(), "the module's sink sends");
    }

    #[test]
    fn a_token_reference_that_is_empty_or_absent_is_refused_at_deploy() {
        for (case, table) in [
            (
                "an empty reference",
                "api_host = \"api.tinybird.co\"\nauction_token_secret = \"\"",
            ),
            (
                "a whitespace reference",
                "api_host = \"api.tinybird.co\"\nauction_token_secret = \" \t \"",
            ),
            ("no reference", "api_host = \"api.tinybird.co\""),
        ] {
            let settings = settings_selecting(table);

            let err = validate_settings_for_deploy_with(&settings, &[builder()]).expect_err(case);

            assert!(
                format!("{err:?}").contains("analytics.tinybird.auction_token_secret"),
                "{case}: the error should identify the token reference: {err:?}"
            );
        }
    }

    #[test]
    fn a_token_key_name_is_accepted_at_deploy() {
        let settings = settings_selecting(
            "api_host = \"api.tinybird.co\"\nauction_token_secret = \"tinybird_auction_token\"",
        );

        validate_settings_for_deploy_with(&settings, &[builder()])
            .expect("should validate the token's key name without treating it as the value");
    }

    /// A document selecting the module by its full name, with `table` as its
    /// settings at `[analytics.tinybird]`.
    fn settings_selecting_the_full_name(table: &str) -> Settings {
        Settings::from_toml(&format!(
            "{}\n[analytics]\nmodule = \"{MODULE}\"\n\n[analytics.tinybird]\n{table}\n",
            crate_test_settings_str()
        ))
        .expect("the document should parse")
    }

    #[test]
    fn the_full_name_s_token_is_checked_at_deploy_in_the_table_the_module_reads() {
        validate_settings_for_deploy_with(&settings_selecting_the_full_name(TABLE), &[builder()])
            .expect("should find the token's key name in [analytics.tinybird]");

        let err = validate_settings_for_deploy_with(
            &settings_selecting_the_full_name("api_host = \"api.tinybird.co\""),
            &[builder()],
        )
        .expect_err("a missing token reference is refused");

        assert!(
            format!("{err:?}").contains("`analytics.tinybird.auction_token_secret`"),
            "should name the token in the table the module reads: {err:?}"
        );
    }

    /// Answers the keys the test document names, and the token's key with
    /// the token.
    struct StoredSecrets;

    impl PlatformSecretStore for StoredSecrets {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            let value = match key {
                "unit-test-proxy-secret" => "unit-test-proxy-secret-32-bytes-ok",
                "test-auction-token" => "resolved-auction-token",
                _ => key,
            };
            Ok(value.as_bytes().to_vec())
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

    #[test]
    fn the_full_name_loads_from_the_stored_form_with_its_token() {
        let mut settings = settings_selecting_the_full_name(TABLE);
        // The blob a push writes names the policy and carries its file.
        settings.permissions = trusted_server_core::test_support::tests::sample_permissions();
        let stored = serde_json::to_value(settings).expect("should serialize the settings");
        let envelope = BlobEnvelope::new(stored, "2026-01-01T00:00:00Z".to_owned());

        let loaded = settings_from_config_blob_with(
            &serde_json::to_string(&envelope).expect("should serialize the envelope"),
            &StoredSecrets,
            &StoreName::from("trusted_server_secrets"),
            trusted_server_core::platform::PolicyOrigin::Blob,
            &[builder()],
        )
        .expect("should load the stored configuration");

        let config = loaded
            .module_config::<TinybirdSettings>(MODULE)
            .expect("should read the table")
            .expect("should select the module");
        assert_eq!(
            config
                .auction_token_secret
                .as_ref()
                .map(|token| token.expose().as_str()),
            Some("resolved-auction-token"),
            "should keep the token's key name in the table the module reads and look it up"
        );
    }

    #[test]
    fn the_documented_block_validates_when_uncommented() {
        let toml = uncomment_block(&template_with_resolved_required_secrets(), "[analytics]");
        let settings = Settings::from_toml(&toml)
            .expect("uncommented [analytics] with documented api_host should parse");

        assert!(
            validate(&settings).expect("the documented block should validate"),
            "the documented block should select the module"
        );
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
