//! Tinybird direct-ingest telemetry for the Fastly adapter.
//!
//! During a request, [`QueueingTelemetrySink`] serializes each auction
//! telemetry batch to NDJSON and queues the body on the request's
//! [`TelemetryQueue`]. After the response is delivered,
//! [`send_queued_batches`] sends each queued body as one Events API request.
//! `EdgeZero` cancels an outbound request whose pending handle is dropped, so a
//! request cannot be left to finish on its own while the response streams.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, header};
use edgezero_core::outbound::{HttpClient, OutboundCachePolicy, ResponseDecoding};
use error_stack::Report;
use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::outbound::{
    DEFAULT_BUFFERED_RESPONSE_BYTES, OutboundRequestSource, buffered, outbound_error,
    outbound_request,
};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::settings::TinybirdSettings;

const TINYBIRD_EVENTS_PATH: &str = "/v0/events";
const TINYBIRD_NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
/// Total timeout for one Events API request.
const TINYBIRD_TIMEOUT: Duration = Duration::from_secs(2);
const TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH: usize = 512;

/// NDJSON bodies one request queued for sending after its response is delivered.
///
/// The Fastly entry point creates one per request and `prepare` inserts a
/// clone into the request extensions, where the per-request services find it.
/// Clones share one queue: [`QueueingTelemetrySink`] pushes onto it during
/// the request, and [`send_queued_batches`] takes the bodies after delivery.
#[derive(Debug, Clone, Default)]
pub(crate) struct TelemetryQueue(Arc<Mutex<Vec<String>>>);

impl TelemetryQueue {
    fn push(&self, body: String) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(body);
    }

    /// Takes every queued body, leaving the queue empty.
    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Auction telemetry sink that queues each batch as NDJSON instead of sending it.
pub(crate) struct QueueingTelemetrySink {
    max_body_bytes: usize,
    queue: TelemetryQueue,
}

impl QueueingTelemetrySink {
    /// Creates a sink that queues on `queue`, bounded by `config`'s body limit.
    pub(crate) fn new(config: &TinybirdSettings, queue: TelemetryQueue) -> Self {
        Self {
            max_body_bytes: config.max_body_bytes,
            queue,
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
}

#[async_trait::async_trait(?Send)]
impl AuctionTelemetrySink for QueueingTelemetrySink {
    async fn emit_auction_events(
        &self,
        _services: &RuntimeServices,
        batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>> {
        if batch.is_empty() {
            return Ok(());
        }

        Self::validate_batch(&batch)?;
        let body = batch.to_ndjson(self.max_body_bytes)?;
        log::info!(
            "queueing auction telemetry for Tinybird rows={} bytes={}",
            batch.row_count(),
            body.len()
        );
        self.queue.push(body);
        Ok(())
    }
}

/// Sends every body queued on `queue` to `config`'s Events API, one request each.
///
/// Best effort: a failed request is logged, followed by `log_context`, and the
/// rest are still sent. Each request carries the 2 s Tinybird timeout and
/// bypasses the cache.
pub(crate) async fn send_queued_batches(
    client: &HttpClient,
    config: &TinybirdSettings,
    queue: &TelemetryQueue,
    log_context: &dyn fmt::Display,
) {
    let bodies = queue.take();
    if bodies.is_empty() {
        return;
    }
    let Some(token) = config.auction_token_secret.as_ref() else {
        log::error!(
            "dropping {} queued auction telemetry batch(es): no Tinybird auction token is resolved{log_context}",
            bodies.len()
        );
        return;
    };
    let uri = tinybird_events_uri(&config.api_host, &config.auction_dataset);

    for body in bodies {
        let bytes = body.len();
        let request = outbound_request(Method::POST, &uri)
            .and_then(|request| {
                request.header(header::AUTHORIZATION, format!("Bearer {}", token.expose()))
            })
            .and_then(|request| request.header(header::CONTENT_TYPE, TINYBIRD_NDJSON_CONTENT_TYPE));
        let request = match request {
            Ok(request) => buffered(
                request.body(body),
                TINYBIRD_TIMEOUT,
                DEFAULT_BUFFERED_RESPONSE_BYTES,
                ResponseDecoding::Original,
            )
            .cache_policy(OutboundCachePolicy::Bypass),
            Err(error) => {
                log_send_failure(
                    error,
                    "the Events API request could not be built".to_owned(),
                    log_context,
                );
                continue;
            }
        };

        log::info!(
            "sending auction telemetry to Tinybird dataset={} bytes={bytes} host={}",
            config.auction_dataset,
            config.api_host
        );
        match client.send(request).await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => log::warn!(
                "Tinybird Events API answered {} for dataset {}{log_context}",
                response.status(),
                config.auction_dataset
            ),
            Err(error) => log_send_failure(
                error,
                format!(
                    "the Events API request for dataset {} failed",
                    config.auction_dataset
                ),
                log_context,
            ),
        }
    }
}

/// Logs one failed Tinybird send at warn, as one line followed by `log_context`.
///
/// The line is the [`outbound_error`] report with the `failed` step. The
/// report carries the failure's budget source and provider detail, never the
/// error's message, which can hold provider text or a URL.
fn log_send_failure(error: EdgeError, failed: String, log_context: &dyn fmt::Display) {
    let report = outbound_error(
        error,
        TrustedServerError::Proxy { message: failed },
        OutboundRequestSource::Internal,
    );
    log::warn!("Tinybird telemetry send failed: {report:?}{log_context}");
}

fn tinybird_events_uri(api_host: &str, dataset: &str) -> String {
    format!(
        "https://{api_host}{TINYBIRD_EVENTS_PATH}?name={}",
        urlencoding::encode(dataset)
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use edgezero_core::http::StatusCode;
    use edgezero_core::outbound::ResponseMode;
    use trusted_server_core::auction::telemetry::AuctionEventRow;
    use trusted_server_core::platform::ClientInfo;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::test_support::logging::LogCapture;
    use trusted_server_core::test_support::scripted_client::{
        ExpectedTransport, ScriptedHttpClient, ScriptedReply, TestClock,
    };

    use super::*;
    use crate::platform::{
        FastlyPlatformConfigStore, FastlyPlatformSecretStore, RequestGeo, UnavailableKvStore,
    };

    const TEST_USER_AGENT: &str =
        "FictionalBrowser/123.4 (FictionalOS 10.2; FictionalDevice) ExampleRenderer/567.8";

    /// Events API URI of [`enabled_config`].
    pub(crate) const EVENTS_URI: &str =
        "https://api.tinybird.example.com/v0/events?name=auction_events_raw";

    pub(crate) fn test_row() -> AuctionEventRow {
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

    pub(crate) fn enabled_config() -> TinybirdSettings {
        TinybirdSettings {
            enabled: true,
            api_host: "api.tinybird.example.com".to_owned(),
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

    /// Services whose outbound requests go through `client`.
    pub(crate) fn test_services(client: &ScriptedHttpClient) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(FastlyPlatformConfigStore::read_only()))
            .secret_store(Arc::new(FastlyPlatformSecretStore::read_only()))
            .kv_store(Arc::new(UnavailableKvStore))
            .outbound_client(client.http_client())
            .clock(TestClock::new().monotonic_clock())
            .geo(Arc::new(RequestGeo::default()))
            .client_info(ClientInfo::default())
            .build()
    }

    fn queue_of(bodies: &[&str]) -> TelemetryQueue {
        let queue = TelemetryQueue::default();
        for body in bodies {
            queue.push((*body).to_owned());
        }
        queue
    }

    /// Asserts `request` is one Events API POST of `body` with today's headers.
    pub(crate) fn assert_events_request(
        request: &trusted_server_core::test_support::scripted_client::RecordedRequest,
        body: &str,
    ) {
        assert_eq!(request.method, Method::POST, "should POST to Tinybird");
        assert_eq!(
            request.uri.to_string(),
            EVENTS_URI,
            "should target the dataset on the region host"
        );
        request.assert_headers(&[
            (header::AUTHORIZATION.as_str(), "Bearer append-token"),
            (header::CONTENT_TYPE.as_str(), TINYBIRD_NDJSON_CONTENT_TYPE),
        ]);
        assert_eq!(
            request.body.as_ref(),
            body.as_bytes(),
            "should send the queued NDJSON body"
        );
        request.assert_transport(ExpectedTransport {
            timeout: TINYBIRD_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            },
            response_decoding: ResponseDecoding::Original,
            cache_policy: OutboundCachePolicy::Bypass,
        });
    }

    #[test]
    fn events_uri_targets_dataset_on_region_host() {
        assert_eq!(
            tinybird_events_uri("api.tinybird.example.com", "auction_events_raw"),
            EVENTS_URI
        );
    }

    #[test]
    fn events_uri_urlencodes_dataset_name() {
        assert_eq!(
            tinybird_events_uri("api.tinybird.example.com", "auction events/raw"),
            "https://api.tinybird.example.com/v0/events?name=auction%20events%2Fraw"
        );
    }

    #[test]
    fn sink_queues_ndjson_with_the_full_user_agent_and_sends_nothing() {
        let client = ScriptedHttpClient::forbidding();
        let queue = TelemetryQueue::default();
        let sink = QueueingTelemetrySink::new(&enabled_config(), queue.clone());

        futures::executor::block_on(sink.emit_auction_events(
            &test_services(&client),
            AuctionEventBatch::new(vec![test_row()]),
        ))
        .expect("should queue the batch");

        client.assert_no_requests();

        let bodies = queue.take();
        let [body] = bodies.as_slice() else {
            panic!("should queue one body, got {bodies:?}");
        };
        assert!(body.ends_with('\n'), "should queue newline-delimited JSON");
        assert!(
            body.contains(TEST_USER_AGENT),
            "should keep the complete user agent for Tinybird"
        );
    }

    #[test]
    fn empty_batch_queues_nothing() {
        let client = ScriptedHttpClient::forbidding();
        let queue = TelemetryQueue::default();
        let sink = QueueingTelemetrySink::new(&enabled_config(), queue.clone());

        futures::executor::block_on(
            sink.emit_auction_events(&test_services(&client), AuctionEventBatch::new(Vec::new())),
        )
        .expect("should ignore the empty batch");

        client.assert_no_requests();
        assert!(queue.take().is_empty(), "should queue no empty batch");
    }

    #[test]
    fn sink_rejects_a_row_count_oversize_before_queueing() {
        let client = ScriptedHttpClient::forbidding();
        let queue = TelemetryQueue::default();
        let sink = QueueingTelemetrySink::new(&enabled_config(), queue.clone());
        let rows = vec![test_row(); TINYBIRD_MAX_ROWS_PER_AUCTION_BATCH + 1];

        let result = futures::executor::block_on(
            sink.emit_auction_events(&test_services(&client), AuctionEventBatch::new(rows)),
        );

        client.assert_no_requests();

        assert!(
            result.is_err(),
            "best-effort caller will suppress this error"
        );
        assert!(
            queue.take().is_empty(),
            "should not queue an oversized row batch"
        );
    }

    #[test]
    fn queued_batches_post_once_each_with_the_token_timeout_and_bypass() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        for _ in 0..2 {
            client.respond_to(
                Method::POST,
                EVENTS_URI,
                ScriptedReply::status(StatusCode::ACCEPTED),
            );
        }
        let queue = queue_of(&["{\"row\":1}\n", "{\"row\":2}\n"]);

        clock.drive(send_queued_batches(
            &client.http_client(),
            &enabled_config(),
            &queue,
            &"",
        ));

        let requests = client.requests();
        let [first, second] = requests.as_slice() else {
            panic!("should send one request per queued body, got {requests:?}");
        };
        assert_events_request(first, "{\"row\":1}\n");
        assert_events_request(second, "{\"row\":2}\n");
        assert!(queue.take().is_empty(), "should drain the queue");
    }

    #[test]
    fn a_failed_batch_does_not_stop_the_next_one() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::POST,
                EVENTS_URI,
                ScriptedReply::error(edgezero_core::error::EdgeError::bad_gateway(
                    "scripted Tinybird failure",
                )),
            )
            .respond_to(
                Method::POST,
                EVENTS_URI,
                ScriptedReply::status(StatusCode::ACCEPTED),
            );
        let queue = queue_of(&["{\"row\":1}\n", "{\"row\":2}\n"]);

        clock.drive(send_queued_batches(
            &client.http_client(),
            &enabled_config(),
            &queue,
            &"",
        ));

        assert_eq!(
            client.requests().len(),
            2,
            "should still send the second body after the first fails"
        );
    }

    #[test]
    fn send_and_build_failures_log_one_line_each() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::POST,
            EVENTS_URI,
            ScriptedReply::error(EdgeError::bad_gateway("tinybird provider text")),
        );
        let mut unbuildable = enabled_config();
        unbuildable.api_host = "api tinybird.example.com".to_owned();
        let capture = LogCapture::start();

        clock.drive(send_queued_batches(
            &client.http_client(),
            &enabled_config(),
            &queue_of(&["{\"row\":1}\n"]),
            &" [request=example]",
        ));
        clock.drive(send_queued_batches(
            &client.http_client(),
            &unbuildable,
            &queue_of(&["{\"row\":2}\n"]),
            &" [request=example]",
        ));

        let warnings = capture.take_all();
        let messages: Vec<&str> = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect();
        assert_eq!(
            messages.len(),
            2,
            "should log one line each for the send failure and the build failure, got {messages:?}"
        );
        for step in [
            "the Events API request for dataset auction_events_raw failed",
            "the Events API request could not be built",
        ] {
            assert!(
                warnings.iter().any(|warning| {
                    warning.level == log::Level::Warn
                        && warning
                            .message
                            .starts_with("Tinybird telemetry send failed:")
                        && warning.message.contains(step)
                        && warning.message.ends_with(" [request=example]")
                }),
                "should log the report for `{step}` with the log context"
            );
        }
        assert!(
            warnings
                .iter()
                .all(|warning| !warning.message.contains("tinybird provider text")),
            "should keep the provider text out of the logs"
        );
        assert_eq!(
            client.requests().len(),
            1,
            "should send only the request that could be built"
        );
    }

    #[test]
    fn an_empty_queue_sends_nothing() {
        let client = ScriptedHttpClient::forbidding();

        futures::executor::block_on(send_queued_batches(
            &client.http_client(),
            &enabled_config(),
            &TelemetryQueue::default(),
            &"",
        ));

        client.assert_no_requests();
    }
}
