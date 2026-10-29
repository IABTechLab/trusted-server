//! Tinybird direct-ingest telemetry sink for the Fastly adapter.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, header, request_builder};
use error_stack::Report;
use trusted_server_core::auction::telemetry::{
    AuctionEventBatch, AuctionTelemetrySink, NoopAuctionTelemetrySink,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{PlatformBackendSpec, PlatformHttpRequest, RuntimeServices};
use trusted_server_core::secret_bindings::{CompiledTarget, ResolvedBinding};
use trusted_server_core::settings::Settings;

const TINYBIRD_NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
const TINYBIRD_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_BETWEEN_BYTES_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH: usize = 512;

/// Build the configured auction telemetry sink from a loaded credential use.
///
/// # Errors
/// Returns a configuration error if enabled telemetry lacks a matching loaded
/// credential binding or a valid compiled target.
pub(crate) fn auction_sink_from_settings(
    settings: &Settings,
) -> Result<Arc<dyn AuctionTelemetrySink>, Report<TrustedServerError>> {
    if !settings.tinybird.enabled {
        return Ok(Arc::new(NoopAuctionTelemetrySink));
    }
    Ok(Arc::new(FastlyTinybirdAuctionTelemetrySink::new(settings)?))
}

#[derive(Debug, Clone)]
struct FastlyTinybirdAuctionTelemetrySink {
    target: TinybirdEventsTarget,
}

#[derive(Debug, Clone)]
struct TinybirdEventsTarget {
    binding: ResolvedBinding,
    uri: String,
    backend_spec: PlatformBackendSpec,
    max_body_bytes: usize,
}

impl TinybirdEventsTarget {
    fn from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let binding = settings
            .resolved_bindings
            .require_for_settings(
                settings,
                "tinybird.auction",
                "tinybird.auction_token_secret",
            )?
            .clone();
        let CompiledTarget::Tinybird { endpoint, dataset } = binding.compiled().target() else {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "tinybird: invalid loaded target".to_owned(),
            }));
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| {
            Report::new(TrustedServerError::Configuration {
                message: "tinybird: invalid loaded endpoint".to_owned(),
            })
        })?;
        let backend_spec = PlatformBackendSpec {
            scheme: "https".to_owned(),
            host: url.host_str().unwrap_or_default().to_owned(),
            port: url.port(),
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: TINYBIRD_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: TINYBIRD_BETWEEN_BYTES_TIMEOUT,
            discriminator: None,
        };
        url.query_pairs_mut().append_pair("name", dataset);
        Ok(Self {
            uri: url.into(),
            binding,
            backend_spec,
            max_body_bytes: settings.tinybird.max_body_bytes,
        })
    }
}

impl FastlyTinybirdAuctionTelemetrySink {
    fn new(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            target: TinybirdEventsTarget::from_settings(settings)?,
        })
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

    fn ensure_backend(
        &self,
        services: &RuntimeServices,
    ) -> Result<String, Report<TrustedServerError>> {
        services
            .backend()
            .ensure(&self.target.backend_spec)
            .map_err(|_| {
                Report::new(TrustedServerError::Proxy {
                    message: "Tinybird backend registration failed".to_owned(),
                })
            })
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
            .map_err(|_| {
                Report::new(TrustedServerError::Proxy {
                    message: "failed to build Tinybird Events API request".to_owned(),
                })
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
            .map_err(|_| {
                Report::new(TrustedServerError::Proxy {
                    message: "failed to start Tinybird Events API request".to_owned(),
                })
            })?;
        drop(pending);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl AuctionTelemetrySink for FastlyTinybirdAuctionTelemetrySink {
    fn is_enabled(&self) -> bool {
        true
    }

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
        let auth_header = self
            .target
            .binding
            .bearer_header()
            .cloned()
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: "tinybird: invalid credential representation".to_owned(),
                })
            })?;
        let request = self.build_events_request(body, auth_header)?;
        let backend_name = self.ensure_backend(services)?;

        log::info!(
            "sending auction telemetry to Tinybird rows={} bytes={}",
            batch.row_count(),
            body_len
        );

        Self::send_fire_and_forget(services, request, backend_name).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use edgezero_core::blob_envelope::BlobEnvelope;
    use error_stack::Report;
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionEventRow};
    use trusted_server_core::config::TrustedServerAppConfig;
    use trusted_server_core::config_payload::settings_from_config_blob;
    use trusted_server_core::platform::{
        ClientInfo, PlatformBackend, PlatformConfigStore, PlatformError, PlatformGeo,
        PlatformHttpClient, PlatformPendingRequest, PlatformResponse, PlatformSecretStore,
        PlatformSelectResult, RuntimeServices, StoreId, StoreName,
    };

    use super::*;

    const TEST_USER_AGENT: &str =
        "FictionalBrowser/123.4 (FictionalOS 10.2; FictionalDevice) ExampleRenderer/567.8";

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
        fn naming_policy(&self) -> trusted_server_core::platform::BackendNamingPolicy {
            trusted_server_core::platform::BackendNamingPolicy::Fastly
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
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo::default())
            .build()
    }

    fn enabled_config() -> Settings {
        enabled_config_for_host("api.us-east.aws.tinybird.co")
    }

    fn enabled_config_for_host(api_host: &str) -> Settings {
        let config: TrustedServerAppConfig = serde_json::from_value(serde_json::json!({
            "publisher": {
                "domain": "publisher.example.com",
                "cookie_domain": ".publisher.example.com",
                "origin_url": "https://origin.publisher.example.com"
            },
            "handlers": [{ "path": "^/_ts/admin", "username": "admin" }],
            "tinybird": {
                "enabled": true,
                "api_host": api_host,
                "auction_dataset": "auction_events_raw",
                "max_body_bytes": 1024 * 1024
            }
        }))
        .expect("should parse the fictional Tinybird feature configuration");
        let plan = trusted_server_core::secret_bindings::compile_bindings(config.settings())
            .expect("should compile the fictional Tinybird credential lookups");
        let store = MapSecretStore(
            plan.entries()
                .iter()
                .map(|entry| {
                    let value = if entry.scope() == "tinybird" {
                        "append-token"
                    } else {
                        "fictional-binding-value-with-32-bytes"
                    };
                    (entry.key().to_owned(), value.as_bytes().to_vec())
                })
                .collect(),
        );
        let data = serde_json::to_value(&config)
            .expect("should serialize the unresolved Tinybird feature configuration");
        let envelope =
            serde_json::to_string(&BlobEnvelope::new(data, "2026-10-26T00:00:00Z".to_owned()))
                .expect("should encode the fictional Tinybird config envelope");
        settings_from_config_blob(
            &envelope,
            &store,
            &StoreName::from(trusted_server_core::secret_bindings::SECRET_STORE_ID),
        )
        .expect("should fully load the fictional Tinybird config and native credentials")
        .settings()
        .clone()
    }

    #[test]
    fn events_uri_targets_dataset_on_region_host() {
        let target = TinybirdEventsTarget::from_settings(&enabled_config())
            .expect("should construct target");
        assert_eq!(
            target.uri,
            "https://api.us-east.aws.tinybird.co/v0/events?name=auction_events_raw"
        );
    }

    #[test]
    fn events_target_uses_compiled_port_and_normalizes_default_https_port() {
        for (authority, expected_uri, expected_port) in [
            (
                "api.example.com:443",
                "https://api.example.com/v0/events?name=auction_events_raw",
                None,
            ),
            (
                "api.example.com:8443",
                "https://api.example.com:8443/v0/events?name=auction_events_raw",
                Some(8443),
            ),
        ] {
            let settings = enabled_config_for_host(authority);
            let target = TinybirdEventsTarget::from_settings(&settings)
                .expect("should construct compiled target");
            assert_eq!(target.uri, expected_uri);
            assert_eq!(target.backend_spec.host, "api.example.com");
            assert_eq!(target.backend_spec.port, expected_port);
            assert!(target.backend_spec.certificate_check);
        }
    }

    #[test]
    fn settings_only_enabled_sink_is_rejected() {
        let mut settings = enabled_config();
        settings.resolved_bindings = Default::default();
        assert!(auction_sink_from_settings(&settings).is_err());
    }

    #[test]
    fn mutable_target_and_dataset_cannot_reuse_loaded_use() {
        let mut settings = enabled_config();
        settings.tinybird.api_host = "other.example.com".to_owned();
        assert!(auction_sink_from_settings(&settings).is_err());
        settings = enabled_config();
        settings.tinybird.auction_dataset = "other_dataset".to_owned();
        assert!(auction_sink_from_settings(&settings).is_err());
    }

    #[test]
    fn backend_spec_uses_matching_tls_host() {
        let target = TinybirdEventsTarget::from_settings(&enabled_config())
            .expect("should construct target");
        let spec = target.backend_spec;
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
        let sink = FastlyTinybirdAuctionTelemetrySink::new(&enabled_config())
            .expect("should construct loaded sink");

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
    fn disabled_sink_does_not_dispatch() {
        let backend = Arc::new(RecordingBackend::default());
        let http_client = Arc::new(RecordingHttpClient::default());
        let services = services(
            Arc::clone(&backend),
            Arc::clone(&http_client),
            HashMap::new(),
        );
        let mut config = enabled_config();
        config.tinybird.enabled = false;
        config.resolved_bindings = Default::default();
        let sink = auction_sink_from_settings(&config).expect("should build disabled sink");

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
        let sink = FastlyTinybirdAuctionTelemetrySink::new(&enabled_config())
            .expect("should construct loaded sink");

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
        let sink = FastlyTinybirdAuctionTelemetrySink::new(&enabled_config())
            .expect("should construct loaded sink");
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
