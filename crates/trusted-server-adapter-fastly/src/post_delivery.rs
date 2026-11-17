//! Work the Fastly entry point runs after a response is delivered.
//!
//! [`run_post_delivery`] sends the request's queued auction telemetry, then
//! runs EC pull sync. Telemetry goes first because its requests are short,
//! while a pull-sync group can take its whole cutoff. Both run only after a
//! committed application body (see [`crate::delivery`]).
//!
//! Both use a fresh `EdgeZero` client and clock, because the request's client
//! went away with the request. They run under
//! [`futures::executor::block_on`] inside the serving callback, after
//! `send_request_with_registries_and_hooks` returned. That is the only safe
//! place: dispatch itself runs under `block_on`, and nesting there panics.
//! `EdgeZero` releases callback-owned batch drivers when the callback returns,
//! so both finish before it does. The client already has its response; each
//! extends only the sandbox's lifetime.

use std::fmt;

use edgezero_adapter_fastly::outbound::FastlyOutboundClient;
use edgezero_core::outbound::HttpClient;
use edgezero_core::time::MonotonicClock;
use error_stack::Report;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::pull_sync::{
    PullSyncContext, build_pull_sync_context, dispatch_pull_sync,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::settings::Settings;

use crate::app::EcFinalizeState;
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use crate::require_identity_graph;
use crate::tinybird::{TelemetryQueue, send_queued_batches};

/// Pull-sync inputs a routed response's finalize step hands to post-delivery work.
pub(crate) struct FinalizeState {
    pub(crate) partner_registry: PartnerRegistry,
    pub(crate) ec_state: EcFinalizeState,
}

/// Sends the queued telemetry, then runs pull sync for `state`, on a fresh client and clock.
///
/// `log_context` ends each failure log, so it matches the delivered response.
pub(crate) fn run_post_delivery(
    state: Option<FinalizeState>,
    queue: &TelemetryQueue,
    settings: &Settings,
    log_context: &dyn fmt::Display,
) {
    let client = post_delivery_client();
    let clock = MonotonicClock::default();
    futures::executor::block_on(post_delivery_work(
        state,
        queue,
        settings,
        &client,
        &clock,
        log_context,
    ));
}

/// A fresh Fastly client for post-delivery work, with the proxy marker off.
///
/// Tinybird and the pull-sync partners never receive `x-edgezero-proxy`.
fn post_delivery_client() -> HttpClient {
    HttpClient::with_client(FastlyOutboundClient::new()).with_proxy_marker(false)
}

/// The post-delivery work itself, on `client` and `clock`: telemetry, then pull sync.
async fn post_delivery_work(
    state: Option<FinalizeState>,
    queue: &TelemetryQueue,
    settings: &Settings,
    client: &HttpClient,
    clock: &MonotonicClock,
    log_context: &dyn fmt::Display,
) {
    send_queued_batches(client, &settings.tinybird, queue, log_context).await;
    if let Some(state) = state {
        pull_sync(state, settings, client, clock, log_context).await;
    }
}

/// Runs pull sync for a real browser whose EC state still has partners to fill.
async fn pull_sync(
    state: FinalizeState,
    settings: &Settings,
    client: &HttpClient,
    clock: &MonotonicClock,
    log_context: &dyn fmt::Display,
) {
    let FinalizeState {
        partner_registry,
        ec_state,
    } = state;
    if !ec_state.is_real_browser {
        return;
    }

    let prepared_context = build_pull_sync_context(&ec_state.ec_context, &partner_registry);
    let Some((context, kv)) = prepare_pull_sync_after_send(
        prepared_context,
        || require_identity_graph(settings),
        log_context,
    ) else {
        return;
    };

    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
    dispatch_pull_sync(
        settings,
        &kv,
        &partner_registry,
        &limiter,
        &context,
        client,
        clock,
    )
    .await;
}

/// Opens the identity graph only when there is a pull-sync context to run.
fn prepare_pull_sync_after_send<F>(
    context: Option<PullSyncContext>,
    graph_factory: F,
    log_context: &dyn fmt::Display,
) -> Option<(PullSyncContext, KvIdentityGraph)>
where
    F: FnOnce() -> Result<KvIdentityGraph, Report<TrustedServerError>>,
{
    let context = context?;
    let kv = match graph_factory() {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("Pull sync: identity graph unavailable, skipping: {err:?}{log_context}");
            return None;
        }
    };
    Some((context, kv))
}

#[cfg(test)]
mod tests {
    use edgezero_adapter_fastly::delivery::DeliveryError;
    use edgezero_core::http::{Method, StatusCode};
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink as _};
    use trusted_server_core::ec::EcContext;
    use trusted_server_core::geo::GeoInfo;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::EcPartner;
    use trusted_server_core::test_support::scripted_client::{
        ScriptedHttpClient, ScriptedReply, TestClock,
    };

    use super::*;
    use crate::delivery::tests::{
        committed, committed_fallback, committed_then_failed, counters, not_committed,
        routed_delivery, unknown_commit,
    };
    use crate::delivery::{DeliveryAction, delivery_action, handle_delivery};
    use crate::tinybird::QueueingTelemetrySink;
    use crate::tinybird::tests::{
        EVENTS_URI, assert_events_request, enabled_config, test_row, test_services,
    };

    fn settings_with_tinybird() -> Settings {
        Settings {
            tinybird: enabled_config(),
            ..Settings::default()
        }
    }

    /// A queue holding the NDJSON the queueing sink wrote for one auction, and that body.
    ///
    /// The sink emits through services built on `client`, so a test can show
    /// that queueing sent nothing.
    fn queue_with_one_auction(client: &ScriptedHttpClient) -> (TelemetryQueue, String) {
        let queue = TelemetryQueue::default();
        let sink = QueueingTelemetrySink::new(&enabled_config(), queue.clone());
        let batch = AuctionEventBatch::new(vec![test_row()]);
        let body = batch
            .to_ndjson(enabled_config().max_body_bytes)
            .expect("should serialize the auction's telemetry");
        futures::executor::block_on(sink.emit_auction_events(&test_services(client), batch))
            .expect("should queue the auction's telemetry");
        (queue, body)
    }

    #[test]
    fn telemetry_rows_queue_during_request_and_send_after_delivery() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::POST,
            EVENTS_URI,
            ScriptedReply::status(StatusCode::ACCEPTED),
        );
        let settings = settings_with_tinybird();
        let (queue, body) = queue_with_one_auction(&client);

        client.assert_no_requests();
        handle_delivery(routed_delivery(committed), &counters(), |_state| {
            clock.drive(post_delivery_work(
                None,
                &queue,
                &settings,
                &client.http_client(),
                &clock.monotonic_clock(),
                &"",
            ));
        });

        let requests = client.requests();
        let [request] = requests.as_slice() else {
            panic!("should send the queued auction once after delivery, got {requests:?}");
        };
        assert_events_request(request, &body);
    }

    #[test]
    fn post_delivery_client_turns_the_proxy_marker_off() {
        assert!(
            !post_delivery_client().proxy_marker_enabled(),
            "should send telemetry and pull-sync requests without the proxy marker"
        );
    }

    #[test]
    fn no_post_delivery_when_not_committed_or_fallback_body() {
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();
        let settings = settings_with_tinybird();
        let (queue, _body) = queue_with_one_auction(&client);
        let run = |_state| {
            clock.drive(post_delivery_work(
                None,
                &queue,
                &settings,
                &client.http_client(),
                &clock.monotonic_clock(),
                &"",
            ));
        };

        handle_delivery(routed_delivery(committed_fallback), &counters(), run);
        handle_delivery(routed_delivery(unknown_commit), &counters(), run);
        assert_eq!(
            delivery_action(routed_delivery(not_committed), &counters()),
            DeliveryAction::SendNativeError,
            "an uncommitted delivery gets a native 500 and no post-delivery work"
        );

        client.assert_no_requests();
        assert_eq!(queue.take().len(), 1, "should leave the telemetry unsent");
    }

    #[test]
    fn post_delivery_without_finalize_state_still_sends_telemetry() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::POST,
            EVENTS_URI,
            ScriptedReply::status(StatusCode::ACCEPTED),
        );
        let (queue, body) = queue_with_one_auction(&client);

        clock.drive(post_delivery_work(
            None,
            &queue,
            &settings_with_tinybird(),
            &client.http_client(),
            &clock.monotonic_clock(),
            &"",
        ));

        let requests = client.requests();
        let [request] = requests.as_slice() else {
            panic!("should send the queued auction once, got {requests:?}");
        };
        assert_events_request(request, &body);
        assert!(queue.take().is_empty(), "should drain the queue");
    }

    #[test]
    fn pull_sync_noop_states_skip_post_send_graph_factory() {
        let calls = std::cell::Cell::new(0);
        let result = prepare_pull_sync_after_send(
            None,
            || {
                calls.set(calls.get() + 1);
                Err(Report::new(TrustedServerError::KvStore {
                    store_name: "unexpected".to_owned(),
                    message: "graph factory should not run".to_owned(),
                }))
            },
            &"",
        );
        assert!(
            result.is_none(),
            "a skipped pull-sync plan should return none"
        );
        assert_eq!(calls.get(), 0, "should not invoke the graph factory");
    }

    #[test]
    fn post_delivery_with_nothing_queued_makes_no_request() {
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

        clock.drive(post_delivery_work(
            None,
            &TelemetryQueue::default(),
            &settings_with_tinybird(),
            &client.http_client(),
            &clock.monotonic_clock(),
            &"",
        ));

        client.assert_no_requests();
    }

    // -----------------------------------------------------------------------
    // Pull sync after delivery, against the local identity store
    // -----------------------------------------------------------------------

    /// EC ID `fastly.toml` seeds, consented, in the local `ec_identity_store`.
    fn seeded_ec_id() -> String {
        format!("{}.test01", "a".repeat(64))
    }

    /// Pull-sync URL the example partner receives for [`seeded_ec_id`].
    fn partner_pull_url() -> String {
        format!("https://sync.example.com/pull?ec_id={}", seeded_ec_id())
    }

    /// Settings with Tinybird, the local identity store, and one pull-sync partner.
    fn pull_sync_settings() -> Settings {
        let mut settings = settings_with_tinybird();
        settings.ec.ec_store = Some("ec_identity_store".to_owned());
        settings.ec.partners = vec![EcPartner {
            name: "Example partner".to_owned(),
            source_domain: "partner.example.com".to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(Redacted::new(
                "example-partner-api-token-32-bytes-ok".to_owned(),
            )),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.example.com/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.example.com".to_owned()],
            pull_sync_ttl_sec: 3600,
            pull_sync_rate_limit: 100,
            ts_pull_token: Some(Redacted::new("example-pull-token".to_owned())),
        }];
        settings
    }

    /// Finalize state for a returning browser whose EC row the local store holds.
    fn returning_reader_state(settings: &Settings) -> FinalizeState {
        let request = edgezero_core::http::request_builder()
            .uri("https://test-publisher.example.com/article")
            .header("cookie", format!("ts-ec={}", seeded_ec_id()))
            .body(edgezero_core::body::Body::empty())
            .expect("should build the reader request");
        let geo = GeoInfo {
            city: "Tokyo".to_owned(),
            country: "JP".to_owned(),
            continent: "Asia".to_owned(),
            latitude: 35.6762,
            longitude: 139.6503,
            metro_code: 0,
            region: None,
            asn: None,
        };
        let mut ec_context = EcContext::read_from_request_with_geo(
            settings,
            &request,
            &test_services(&ScriptedHttpClient::forbidding()),
            Some(&geo),
        )
        .expect("should read the reader's EC context");
        let kv = require_identity_graph(settings).expect("should open the local identity store");
        ec_context.set_kv_snapshot(kv.load_snapshot(&seeded_ec_id()));
        FinalizeState {
            partner_registry: PartnerRegistry::from_config(&settings.ec.partners)
                .expect("should build the partner registry"),
            ec_state: EcFinalizeState {
                ec_context,
                use_finalize_kv: false,
                eids_cookie: None,
                sharedid_cookie: None,
                is_real_browser: true,
            },
        }
    }

    #[test]
    fn pull_sync_reaches_partner_after_origin_aborts_mid_body() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::POST,
                EVENTS_URI,
                ScriptedReply::status(StatusCode::ACCEPTED),
            )
            .respond_to(
                Method::GET,
                &partner_pull_url(),
                ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"example-partner-uid"}"#),
            );
        let settings = pull_sync_settings();
        let (queue, _body) = queue_with_one_auction(&client);
        let state = returning_reader_state(&settings);

        // The origin aborted after the page head was committed.
        handle_delivery(
            routed_delivery(|envelope| committed_then_failed(envelope, DeliveryError::Source)),
            &counters(),
            |_routed| {
                clock.drive(post_delivery_work(
                    Some(state),
                    &queue,
                    &settings,
                    &client.http_client(),
                    &clock.monotonic_clock(),
                    &"",
                ));
            },
        );

        let requested: Vec<(Method, String)> = client
            .requests()
            .iter()
            .map(|request| (request.method.clone(), request.uri.to_string()))
            .collect();
        assert_eq!(
            requested,
            vec![
                (Method::POST, EVENTS_URI.to_owned()),
                (Method::GET, partner_pull_url()),
            ],
            "should send the telemetry first, then reach the pull-sync partner"
        );
    }

    #[test]
    fn framing_fallback_delivery_makes_no_pull_sync_call() {
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();
        let settings = pull_sync_settings();
        let state = returning_reader_state(&settings);
        let mut state = Some(state);

        handle_delivery(
            routed_delivery(committed_fallback),
            &counters(),
            |_routed| {
                clock.drive(post_delivery_work(
                    state.take(),
                    &TelemetryQueue::default(),
                    &settings,
                    &client.http_client(),
                    &clock.monotonic_clock(),
                    &"",
                ));
            },
        );

        client.assert_no_requests();
        assert!(
            state.is_some(),
            "should never run post-delivery work for a delivered framing fallback"
        );
    }
}
