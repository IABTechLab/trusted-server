//! Tinybird direct-ingest telemetry sink for the Fastly adapter.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, header, request_builder};
use error_stack::{Report, ResultExt as _};
use trusted_server_core::auction::telemetry::{
    AuctionEventBatch, AuctionTelemetrySink, NoopAuctionTelemetrySink,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{
    PlatformBackend as _, PlatformBackendSpec, PlatformHttpClient, PlatformHttpRequest,
    PlatformSecretStore as _, RuntimeServices, StoreName,
};
use trusted_server_core::settings::{Settings, TinybirdSettings};

use crate::platform::{FastlyPlatformBackend, FastlyPlatformSecretStore};

const TINYBIRD_EVENTS_PATH: &str = "/v0/events";
const TINYBIRD_NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
const TINYBIRD_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_BETWEEN_BYTES_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH: usize = 512;

/// Build the configured auction telemetry sink.
///
/// Auction emission requires both the Tinybird master toggle
/// (`tinybird.enabled`) and the auction-specific toggle
/// (`tinybird.auction_enabled`), so access-log telemetry can be enabled
/// independently without also emitting auction events.
#[must_use]
pub(crate) fn auction_sink_from_settings(settings: &Settings) -> Arc<dyn AuctionTelemetrySink> {
    if settings.tinybird.enabled && settings.tinybird.auction_enabled {
        Arc::new(FastlyTinybirdAuctionTelemetrySink::new(
            settings.tinybird.clone(),
        ))
    } else {
        Arc::new(NoopAuctionTelemetrySink)
    }
}

#[derive(Debug, Clone)]
struct FastlyTinybirdAuctionTelemetrySink {
    enabled: bool,
    target: TinybirdEventsTarget,
}

#[derive(Debug, Clone)]
pub(crate) struct TinybirdEventsTarget {
    api_host: String,
    dataset: String,
    secret_store: StoreName,
    token_secret: String,
    uri: String,
    backend_spec: PlatformBackendSpec,
    max_body_bytes: usize,
}

impl TinybirdEventsTarget {
    fn from_config(config: TinybirdSettings) -> Self {
        let uri = tinybird_events_uri(&config.api_host, &config.auction_dataset);
        let backend_spec = tinybird_backend_spec(&config.api_host);
        Self {
            api_host: config.api_host,
            dataset: config.auction_dataset,
            secret_store: StoreName::from(config.secret_store),
            token_secret: config.auction_token_secret,
            uri,
            backend_spec,
            max_body_bytes: config.max_body_bytes,
        }
    }

    /// Builds the Events API target for the access-log datasource.
    ///
    /// Shares [`from_config`](Self::from_config)'s host/secret-store/
    /// body-size-limit derivation, but points at `access_dataset` and
    /// `access_token_secret` instead of the auction pair, so access-log
    /// emission never shares a datasource or token with auction telemetry
    /// even though both configs come from the same [`TinybirdSettings`].
    pub(crate) fn from_access_config(config: TinybirdSettings) -> Self {
        let uri = tinybird_events_uri(&config.api_host, &config.access_dataset);
        let backend_spec = tinybird_backend_spec(&config.api_host);
        Self {
            api_host: config.api_host,
            dataset: config.access_dataset,
            secret_store: StoreName::from(config.secret_store),
            token_secret: config.access_token_secret,
            uri,
            backend_spec,
            max_body_bytes: config.max_body_bytes,
        }
    }
}

impl FastlyTinybirdAuctionTelemetrySink {
    fn new(config: TinybirdSettings) -> Self {
        let enabled = config.enabled;
        Self {
            enabled,
            target: TinybirdEventsTarget::from_config(config),
        }
    }

    fn validate_batch(batch: &AuctionEventBatch) -> Result<(), Report<TrustedServerError>> {
        if batch.row_count() > TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH {
            return Err(Report::new(TrustedServerError::Proxy {
                message: format!(
                    "auction telemetry batch has {} rows, exceeding {} row limit",
                    batch.row_count(),
                    TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH
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

    fn load_append_token(
        &self,
        services: &RuntimeServices,
    ) -> Result<String, Report<TrustedServerError>> {
        let token = services
            .secret_store()
            .get_string(&self.target.secret_store, &self.target.token_secret)
            .change_context(TrustedServerError::Proxy {
                message: "Tinybird auction append token unavailable".to_owned(),
            })?;
        let token = token.trim().to_owned();
        if token.is_empty() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "Tinybird auction append token is empty".to_owned(),
            }));
        }
        Ok(token)
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
            .header(header::CONTENT_TYPE, TINYBIRD_NDJSON_CONTENT_TYPE)
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
impl AuctionTelemetrySink for FastlyTinybirdAuctionTelemetrySink {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn emit_auction_events(
        &self,
        services: &RuntimeServices,
        batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>> {
        if !self.enabled || batch.is_empty() {
            return Ok(());
        }

        Self::validate_batch(&batch)?;
        let body = self.serialize_batch(&batch)?;
        let body_len = body.len();
        let token = self.load_append_token(services)?;
        let auth_header = Self::authorization_header(&token)?;
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

// ---------------------------------------------------------------------------
// Access telemetry: confirmed-delivery emitter
// ---------------------------------------------------------------------------

/// Bucket count [`sampled_in`] maps `entropy` into.
///
/// Large enough that `rate` values with several significant digits (e.g.
/// `0.015`) still land in a distinct bucket instead of rounding away, while
/// staying well inside `u64` range once multiplied by `rate`.
const ACCESS_SAMPLE_BUCKETS: u64 = 1_000_000;

/// Decides whether one request's access-telemetry row should be emitted.
///
/// `entropy` should vary from request to request — callers derive it from
/// the wall-clock event timestamp `XORed` with a cheap per-request value (see
/// the call site in `main.rs`). There is no `rand` crate dependency here:
/// the wasm32-wasip1 guest has no equivalent to `Math.random()`. Mapping
/// `entropy % ACCESS_SAMPLE_BUCKETS` into `[0, 1)` and comparing against
/// `rate` is not cryptographically uniform (the low bits of a timestamp are
/// not perfectly evenly distributed), but access-telemetry sampling only
/// needs an approximately even sample, not a provably unbiased one.
///
/// `rate <= 0.0` always returns `false` and `rate >= 1.0` always returns
/// `true`, independent of `entropy`, so both boundary configurations behave
/// predictably. `0.0` cannot actually occur while `access_enabled` is `true`
/// (`Settings` validation requires `access_sample_rate > 0.0` in that case),
/// but this function stays total rather than leaning on that invariant.
#[must_use]
pub(crate) fn sampled_in(rate: f64, entropy: u64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    let threshold = (rate * ACCESS_SAMPLE_BUCKETS as f64) as u64;
    entropy % ACCESS_SAMPLE_BUCKETS < threshold
}

/// Loads and validates the access-log APPEND token from the Fastly secret store.
///
/// Constructs [`FastlyPlatformSecretStore`] directly instead of routing
/// through [`RuntimeServices`]: access-telemetry emission runs post-delivery
/// for every response class — including asset, admin, and error responses
/// that never build a route-scoped `RuntimeServices` — so the transport
/// context here must be adapter-owned and route-independent rather than
/// threaded from wherever the route happened to construct one.
fn load_access_token(target: &TinybirdEventsTarget) -> Result<String, Report<TrustedServerError>> {
    let token = FastlyPlatformSecretStore
        .get_string(&target.secret_store, &target.token_secret)
        .change_context(TrustedServerError::Proxy {
            message: "Tinybird access append token unavailable".to_owned(),
        })?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "Tinybird access append token is empty".to_owned(),
        }));
    }
    Ok(token)
}

/// Builds the Events API POST request for one access-log row.
fn build_access_events_request(
    target: &TinybirdEventsTarget,
    body: String,
    auth_header: HeaderValue,
) -> Result<edgezero_core::http::Request, Report<TrustedServerError>> {
    request_builder()
        .method(Method::POST)
        .uri(target.uri.as_str())
        .header(header::AUTHORIZATION, auth_header)
        .header(header::CONTENT_TYPE, TINYBIRD_NDJSON_CONTENT_TYPE)
        .body(Body::from(body))
        .change_context(TrustedServerError::Proxy {
            message: "failed to build Tinybird Events API request".to_owned(),
        })
}

/// Sends one confirmed access-log row to the Tinybird Events API and waits
/// for the response.
///
/// Unlike [`FastlyTinybirdAuctionTelemetrySink::emit_auction_events`] (fire-
/// and-forget, dispatched mid-request so it never adds latency to the
/// response), this runs post-delivery: the response has already reached the
/// client, so there is no latency budget left to protect, and the send can
/// afford to wait for — and validate — the reply. `client` is the adapter's
/// stateless platform HTTP client in production
/// ([`crate::platform::FastlyPlatformHttpClient`]); accepting it as `&dyn
/// PlatformHttpClient` here (rather than that concrete type) is what lets
/// tests substitute a recording double instead of performing a real network
/// send, matching how [`RuntimeServices::http_client`] is consumed
/// elsewhere. `target` is derived from settings once at the post-send call
/// site rather than threaded through any per-route state.
///
/// A non-2xx status is reported as `Err` naming the status; there is no
/// retry — the caller logs exactly one warning and moves on.
///
/// # Errors
///
/// Returns `Err` when the access-log APPEND token cannot be loaded, the
/// backend cannot be registered, the request cannot be built or sent, or the
/// Tinybird Events API responds with a non-2xx status.
pub(crate) async fn emit_access_event(
    client: &dyn PlatformHttpClient,
    target: &TinybirdEventsTarget,
    row: String,
) -> Result<(), Report<TrustedServerError>> {
    let token = load_access_token(target)?;
    let auth_header = FastlyTinybirdAuctionTelemetrySink::authorization_header(&token)?;
    let backend_name = FastlyPlatformBackend
        .ensure(&target.backend_spec)
        .change_context(TrustedServerError::Proxy {
            message: "Tinybird backend registration failed".to_owned(),
        })?;
    let request = build_access_events_request(target, row, auth_header)?;

    log::info!(
        "sending access telemetry to Tinybird dataset={} host={} backend={}",
        target.dataset,
        target.api_host,
        backend_name
    );

    let response = client
        .send(PlatformHttpRequest::new(request, backend_name))
        .await
        .change_context(TrustedServerError::Proxy {
            message: "failed to send Tinybird access telemetry request".to_owned(),
        })?;

    if response.response.status().is_success() {
        Ok(())
    } else {
        Err(Report::new(TrustedServerError::Proxy {
            message: format!(
                "Tinybird access telemetry request failed with status {}",
                response.response.status()
            ),
        }))
    }
}

fn tinybird_backend_spec(api_host: &str) -> PlatformBackendSpec {
    PlatformBackendSpec {
        scheme: "https".to_owned(),
        host: api_host.to_owned(),
        port: None,
        host_header_override: None,
        certificate_check: true,
        first_byte_timeout: TINYBIRD_FIRST_BYTE_TIMEOUT,
        between_bytes_timeout: TINYBIRD_BETWEEN_BYTES_TIMEOUT,
        discriminator: None,
    }
}

fn tinybird_events_uri(api_host: &str, dataset: &str) -> String {
    format!(
        "https://{api_host}{TINYBIRD_EVENTS_PATH}?name={}",
        urlencoding::encode(dataset)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use error_stack::Report;
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionEventRow};
    use trusted_server_core::platform::{
        ClientInfo, PlatformBackend, PlatformConfigStore, PlatformError, PlatformGeo,
        PlatformHttpClient, PlatformPendingRequest, PlatformResponse, PlatformSecretStore,
        PlatformSelectResult, RuntimeServices, StoreId,
    };

    use super::*;

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

    /// Records outbound requests and, for [`PlatformHttpClient::send`] (the
    /// blocking variant `emit_access_event` uses), returns a synthetic
    /// response carrying `respond_status` instead of performing a real
    /// network send.
    #[derive(Default)]
    struct RecordingHttpClient {
        requests: Mutex<Vec<RecordedRequest>>,
        select_calls: Mutex<usize>,
        respond_status: Mutex<u16>,
    }

    impl RecordingHttpClient {
        /// Status [`PlatformHttpClient::send`] should reply with. Irrelevant
        /// to auction-sink tests, which only exercise `send_async`.
        fn respond_with(status: u16) -> Self {
            Self {
                respond_status: Mutex::new(status),
                ..Self::default()
            }
        }

        fn record(&self, request: PlatformHttpRequest) {
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
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for RecordingHttpClient {
        async fn send(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            self.record(request);
            let status = *self
                .respond_status
                .lock()
                .expect("should lock configured response status");
            let response = edgezero_core::http::response_builder()
                .status(
                    edgezero_core::http::StatusCode::from_u16(status)
                        .expect("should build a valid test status code"),
                )
                .body(edgezero_core::body::Body::empty())
                .expect("should build test response");
            Ok(PlatformResponse::new(response))
        }

        async fn send_async(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            self.record(request);
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

    impl PlatformGeo for NoopGeo {
        fn lookup(
            &self,
            _client_ip: Option<std::net::IpAddr>,
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
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo::default())
            .build()
    }

    fn enabled_config() -> TinybirdSettings {
        TinybirdSettings {
            enabled: true,
            auction_enabled: true,
            api_host: "api.us-east.aws.tinybird.co".to_owned(),
            secret_store: "ts_secrets".to_owned(),
            auction_dataset: "auction_events_raw".to_owned(),
            auction_token_secret: "tinybird_auction_append_token".to_owned(),
            access_enabled: false,
            access_dataset: "access_logs_raw".to_owned(),
            access_token_secret: "tinybird_access_append_token".to_owned(),
            access_sample_rate: 0.0,
            max_body_bytes: 1024 * 1024,
        }
    }

    #[test]
    fn sink_from_settings_disables_when_auction_enabled_is_false() {
        let settings = Settings {
            tinybird: TinybirdSettings {
                auction_enabled: false,
                ..enabled_config()
            },
            ..Settings::default()
        };

        let sink = auction_sink_from_settings(&settings);

        assert!(
            !sink.is_enabled(),
            "auction telemetry should stay off when auction_enabled is false, even if tinybird.enabled is true"
        );
    }

    #[test]
    fn sink_from_settings_enables_when_both_toggles_are_true() {
        let settings = Settings {
            tinybird: enabled_config(),
            ..Settings::default()
        };

        let sink = auction_sink_from_settings(&settings);

        assert!(
            sink.is_enabled(),
            "auction telemetry should be on when both tinybird.enabled and tinybird.auction_enabled are true"
        );
    }

    #[test]
    fn events_uri_targets_dataset_on_region_host() {
        assert_eq!(
            tinybird_events_uri("api.us-east.aws.tinybird.co", "auction_events_raw"),
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction_events_raw"
        );
    }

    #[test]
    fn events_uri_urlencodes_dataset_name() {
        assert_eq!(
            tinybird_events_uri("api.us-east.aws.tinybird.co", "auction events/raw"),
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction%20events%2Fraw"
        );
    }

    #[test]
    fn backend_spec_uses_matching_tls_host() {
        let spec = tinybird_backend_spec("api.us-east.aws.tinybird.co");
        assert_eq!(spec.scheme, "https");
        assert_eq!(spec.host, "api.us-east.aws.tinybird.co");
        assert_eq!(spec.host_header_override, None);
        assert!(spec.certificate_check, "should verify Tinybird TLS cert");
    }

    #[test]
    fn sink_posts_ndjson_with_secret_token_and_does_not_wait() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            Arc::clone(&backend),
            Arc::clone(&http_client),
            HashMap::from([(
                "tinybird_auction_append_token".to_owned(),
                b" append-token\n".to_vec(),
            )]),
        );
        let sink = FastlyTinybirdAuctionTelemetrySink::new(enabled_config());

        futures::executor::block_on(
            sink.emit_auction_events(&services, AuctionEventBatch::new(vec![test_row()])),
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
            Some(TINYBIRD_NDJSON_CONTENT_TYPE)
        );
        assert_eq!(
            header_value(&requests[0].headers, header::AUTHORIZATION.as_str()),
            Some("Bearer append-token")
        );
        assert!(
            std::str::from_utf8(&requests[0].body)
                .expect("should record utf8 ndjson body")
                .ends_with('\n'),
            "should send newline-delimited JSON"
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
    fn disabled_sink_does_not_dispatch() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            Arc::clone(&backend),
            Arc::clone(&http_client),
            HashMap::new(),
        );
        let mut config = enabled_config();
        config.enabled = false;
        let sink = FastlyTinybirdAuctionTelemetrySink::new(config);

        futures::executor::block_on(
            sink.emit_auction_events(&services, AuctionEventBatch::new(vec![test_row()])),
        )
        .expect("should ignore disabled sink");

        assert!(
            backend.specs.lock().expect("should lock specs").is_empty(),
            "should not ensure backend when disabled"
        );
        assert!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .is_empty(),
            "should not send when disabled"
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
        let sink = FastlyTinybirdAuctionTelemetrySink::new(enabled_config());

        futures::executor::block_on(
            sink.emit_auction_events(&services, AuctionEventBatch::new(Vec::new())),
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
    fn sink_drops_missing_secret_as_setup_error() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(backend, Arc::clone(&http_client), HashMap::new());
        let sink = FastlyTinybirdAuctionTelemetrySink::new(enabled_config());

        let result = futures::executor::block_on(
            sink.emit_auction_events(&services, AuctionEventBatch::new(vec![test_row()])),
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
            "should not send without a token"
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
        let sink = FastlyTinybirdAuctionTelemetrySink::new(enabled_config());
        let rows = vec![test_row(); TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH + 1];

        let result = futures::executor::block_on(
            sink.emit_auction_events(&services, AuctionEventBatch::new(rows)),
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
    fn access_emitter_posts_ndjson_and_validates_2xx() {
        // `ts_secrets`/`tinybird_access_append_token` is seeded in
        // fastly.toml's `[local_server.secret_stores]` fixture (value
        // "test-tinybird-access-append-token"), so `emit_access_event` can
        // load a real token through Viceroy without a secret-store test
        // double — the same fixture backs the auction-token secret used
        // above.
        let target = TinybirdEventsTarget::from_access_config(enabled_config());
        let http_client = RecordingHttpClient::respond_with(202);
        let row = r#"{"status":200}"#.to_owned();

        futures::executor::block_on(emit_access_event(&http_client, &target, row.clone()))
            .expect("should accept a 202 response");

        let requests = http_client
            .requests
            .lock()
            .expect("should lock recorded requests");
        assert_eq!(requests.len(), 1, "should send exactly one request");
        assert_eq!(
            requests[0].uri,
            "https://api.us-east.aws.tinybird.co/v0/events?name=access_logs_raw"
        );
        assert_eq!(requests[0].method, Method::POST.to_string());
        assert_eq!(
            header_value(&requests[0].headers, header::AUTHORIZATION.as_str()),
            Some("Bearer test-tinybird-access-append-token")
        );
        assert_eq!(
            std::str::from_utf8(&requests[0].body).expect("should record utf8 body"),
            row,
            "should send the row verbatim as the request body"
        );
    }

    #[test]
    fn access_emitter_warns_and_drops_on_non_2xx() {
        let target = TinybirdEventsTarget::from_access_config(enabled_config());
        let http_client = RecordingHttpClient::respond_with(422);

        let result = futures::executor::block_on(emit_access_event(
            &http_client,
            &target,
            r#"{"status":422}"#.to_owned(),
        ));

        let error = result.expect_err("a 422 response should be reported as an error");
        assert!(
            error.to_string().contains("422"),
            "error should name the failing status: {error}"
        );
        assert_eq!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .len(),
            1,
            "should not retry after a non-2xx response"
        );
    }

    #[test]
    fn sampled_out_requests_emit_nothing() {
        // Mirrors main.rs's post-send gate exactly (`if sampled_in(rate,
        // entropy) { emit_access_event(...) }`): `emit_access_event` is only
        // reached when `sampled_in` returns `true`. With a `0.0` rate it
        // never does, for any entropy, so the http client should never see
        // a request.
        let http_client = RecordingHttpClient::respond_with(202);
        let target = TinybirdEventsTarget::from_access_config(enabled_config());
        let rate = 0.0;
        let entropy = 123_456_789_u64;

        if sampled_in(rate, entropy) {
            futures::executor::block_on(emit_access_event(&http_client, &target, "{}".to_owned()))
                .expect("should send when sampled in");
        }

        assert_eq!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .len(),
            0,
            "sampled-out requests must never reach emit_access_event"
        );
    }

    #[test]
    fn sampled_in_boundary_rates_are_unconditional() {
        assert!(
            sampled_in(1.0, 0),
            "a 1.0 sample rate should always sample in"
        );
        assert!(
            sampled_in(1.0, u64::MAX),
            "a 1.0 sample rate should always sample in regardless of entropy"
        );
        assert!(
            !sampled_in(0.0, 0),
            "a 0.0 sample rate should never sample in"
        );
        assert!(
            !sampled_in(0.0, u64::MAX),
            "a 0.0 sample rate should never sample in regardless of entropy"
        );
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
