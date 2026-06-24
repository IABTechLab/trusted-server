//! Tinybird direct-ingest telemetry sink for the Fastly adapter.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{header, request_builder, HeaderValue, Method};
use error_stack::{Report, ResultExt as _};
use trusted_server_core::auction::telemetry::{
    AuctionEventBatch, AuctionTelemetrySink, NoopAuctionTelemetrySink,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{
    PlatformBackendSpec, PlatformHttpRequest, RuntimeServices, StoreName,
};
use trusted_server_core::settings::{Settings, TinybirdSettings};

const TINYBIRD_EVENTS_PATH: &str = "/v0/events";
const TINYBIRD_NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
const TINYBIRD_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH: usize = 512;

/// Build the configured auction telemetry sink.
#[must_use]
pub(crate) fn auction_sink_from_settings(settings: &Settings) -> Arc<dyn AuctionTelemetrySink> {
    if settings.tinybird.enabled {
        Arc::new(FastlyTinybirdAuctionTelemetrySink::new(
            settings.tinybird.clone(),
        ))
    } else {
        Arc::new(NoopAuctionTelemetrySink)
    }
}

#[derive(Debug, Clone)]
struct FastlyTinybirdAuctionTelemetrySink {
    config: TinybirdSettings,
}

impl FastlyTinybirdAuctionTelemetrySink {
    fn new(config: TinybirdSettings) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait(?Send)]
impl AuctionTelemetrySink for FastlyTinybirdAuctionTelemetrySink {
    async fn emit_auction_events(
        &self,
        services: &RuntimeServices,
        batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>> {
        if !self.config.enabled || batch.is_empty() {
            return Ok(());
        }
        if batch.row_count() > TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH {
            return Err(Report::new(TrustedServerError::Proxy {
                message: format!(
                    "auction telemetry batch has {} rows, exceeding {} row limit",
                    batch.row_count(),
                    TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH
                ),
            }));
        }

        let body = batch.to_ndjson(self.config.max_body_bytes)?;
        let token = services
            .secret_store()
            .get_string(
                &StoreName::from(self.config.secret_store.as_str()),
                &self.config.auction_token_secret,
            )
            .change_context(TrustedServerError::Proxy {
                message: "Tinybird auction append token unavailable".to_owned(),
            })?;
        let token = token.trim();
        if token.is_empty() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "Tinybird auction append token is empty".to_owned(),
            }));
        }

        let backend_name = services
            .backend()
            .ensure(&tinybird_backend_spec(&self.config.api_host))
            .change_context(TrustedServerError::Proxy {
                message: "Tinybird backend registration failed".to_owned(),
            })?;

        let uri = tinybird_events_uri(&self.config.api_host, &self.config.auction_dataset);
        let auth_header = HeaderValue::from_str(&format!("Bearer {token}")).change_context(
            TrustedServerError::InvalidHeaderValue {
                message: "invalid Tinybird authorization header".to_owned(),
            },
        )?;
        let request = request_builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::AUTHORIZATION, auth_header)
            .header(header::CONTENT_TYPE, TINYBIRD_NDJSON_CONTENT_TYPE)
            .body(Body::from(body))
            .change_context(TrustedServerError::Proxy {
                message: "failed to build Tinybird Events API request".to_owned(),
            })?;

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

fn tinybird_backend_spec(api_host: &str) -> PlatformBackendSpec {
    PlatformBackendSpec {
        scheme: "https".to_owned(),
        host: api_host.to_owned(),
        port: None,
        host_header_override: None,
        certificate_check: true,
        first_byte_timeout: TINYBIRD_FIRST_BYTE_TIMEOUT,
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
    fn events_uri_targets_dataset_on_region_host() {
        assert_eq!(
            tinybird_events_uri("api.us-east.aws.tinybird.co", "auction_events_raw"),
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction_events_raw"
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

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
