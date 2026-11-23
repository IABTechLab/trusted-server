//! Work the Fastly entry point runs after a response is delivered.
//!
//! [`run_post_delivery`] sends the request's queued auction telemetry, then
//! runs the navigation's after-response identity lookups, pull sync among
//! them. Telemetry goes first because its requests are short. Both run only
//! after a committed application body (see [`crate::delivery`]).
//!
//! They use a fresh `EdgeZero` client, because the request's client went away
//! with the request. They run under [`futures::executor::block_on`] inside
//! the serving callback, after `send_request_with_registries_and_hooks`
//! returned. That is the only safe place: dispatch itself runs under
//! `block_on`, and nesting there panics. `EdgeZero` releases callback-owned
//! batch drivers when the callback returns, so both finish before it does.
//! The client already has its response; each extends only the sandbox's
//! lifetime.

use std::fmt;
use std::sync::Arc;

use edgezero_adapter_fastly::outbound::FastlyOutboundClient;
use edgezero_core::outbound::HttpClient;
use trusted_server_core::ec::after_response::{
    after_response_lookups_allowed, commit_after_response_lookups,
};
use trusted_server_core::ec::app::IdentityApp;
use trusted_server_core::ec::coordinator::transport::OutboundLookupTransport;
use trusted_server_core::ec::coordinator::{IdentityCoordinator as _, LookupContext};
use trusted_server_core::ec::current_timestamp;
use trusted_server_core::ec::eid_types::UnixSeconds;
use trusted_server_core::ec::provider::{ClientSignals, RequestCookies};
use trusted_server_core::settings::Settings;

use crate::app::{AfterResponseLookups, EcFinalizeState};
use crate::tinybird::{TelemetryQueue, send_queued_batches};

/// Inputs a routed response's finalize step hands to post-delivery work.
pub(crate) struct FinalizeState {
    /// The identity app built once with the retained application state.
    pub(crate) identity: Arc<IdentityApp>,
    pub(crate) ec_state: EcFinalizeState,
    /// The navigation's after-response identity lookups, if any.
    pub(crate) after_response: Option<AfterResponseLookups>,
}

/// Sends the queued telemetry, then runs after-response lookups for `state`,
/// on a fresh client.
///
/// `log_context` ends each failure log, so it matches the delivered response.
pub(crate) fn run_post_delivery(
    state: Option<FinalizeState>,
    queue: &TelemetryQueue,
    settings: &Settings,
    log_context: &dyn fmt::Display,
) {
    let client = post_delivery_client();
    futures::executor::block_on(post_delivery_work(
        state,
        queue,
        settings,
        &client,
        log_context,
    ));
}

/// A fresh Fastly client for post-delivery work, with the proxy marker off.
///
/// Tinybird and the after-response providers, pull-sync partners included,
/// never receive `x-edgezero-proxy`.
fn post_delivery_client() -> HttpClient {
    HttpClient::with_client(FastlyOutboundClient::new()).with_proxy_marker(false)
}

/// The post-delivery work itself, on `client`: telemetry, then
/// after-response lookups.
async fn post_delivery_work(
    state: Option<FinalizeState>,
    queue: &TelemetryQueue,
    settings: &Settings,
    client: &HttpClient,
    log_context: &dyn fmt::Display,
) {
    send_queued_batches(client, &settings.tinybird, queue, log_context).await;
    if let Some(state) = state {
        let FinalizeState {
            identity,
            mut ec_state,
            after_response,
        } = state;
        run_after_response_lookups(&mut ec_state, after_response, &identity, settings, client)
            .await;
    }
}

/// Runs after-response identity lookups and applies their follow-up write.
///
/// Runs once the response has been delivered, so provider latency never
/// delays the page. Skipped unless finalize confirmed the navigation (it
/// issues the follow-up permit) and the EC is still the one the lookups were
/// prepared for. Lookups go out on the post-delivery `client`.
pub(crate) async fn run_after_response_lookups(
    ec_state: &mut EcFinalizeState,
    lookups: Option<AfterResponseLookups>,
    identity: &IdentityApp,
    settings: &Settings,
    client: &HttpClient,
) {
    let Some(lookups) = lookups else {
        return;
    };
    let Some(permit) = ec_state.ec_context.take_after_response_permit() else {
        log::debug!(
            "After-response lookups skipped: no permit (consent, redirect or failed page-load write)"
        );
        return;
    };
    if ec_state.ec_context.ec_value() != Some(lookups.ec_id.as_str()) {
        log::debug!("After-response lookups skipped: EC changed during the request");
        return;
    }
    // The request snapshot holds the row as finalize left it. A source held
    // there is still held in the live row unless consent was withdrawn, which
    // blocks the lookup anyway, so the live read is skipped when nothing is due.
    // Only pull sync can be skipped: configured providers always run, so their
    // cookie opt-outs and regional exclusions are processed.
    let now = UnixSeconds::new(current_timestamp());
    let request_entry = ec_state.ec_context.kv_snapshot().entry_for(&lookups.ec_id);
    if !lookups.coordinator.any_due(request_entry, now) {
        log::debug!("After-response lookups skipped: nothing due");
        return;
    }
    let Some(graph) = identity.identity_graph() else {
        return;
    };
    let snapshot = graph.load_snapshot(&lookups.ec_id).await;
    if !after_response_lookups_allowed(
        &snapshot,
        &lookups.ec_id,
        ec_state.ec_context.ec_generated(),
    ) {
        log::debug!("After-response lookups skipped: new EC or no live row");
        return;
    }
    let transport = OutboundLookupTransport::new(client);
    let pending = lookups
        .coordinator
        .dispatch(
            &transport,
            &LookupContext {
                ec_id: &lookups.ec_id,
                snapshot: &snapshot,
                minted_entry: None,
                consent: ec_state.ec_context.consent(),
                client: ClientSignals {
                    ip: lookups.client_ip,
                    user_agent: lookups.user_agent.as_deref(),
                },
                publisher_domain: &settings.publisher.domain,
                cookies: RequestCookies::new(&lookups.cookie_header),
                rate_limiter: identity.rate_limiter(),
                now,
            },
        )
        .await;
    if pending.is_empty() {
        return;
    }
    let results = lookups.coordinator.collect(&transport, pending).await;
    let outcome = commit_after_response_lookups(
        &graph,
        permit,
        identity.partners(),
        &lookups.ec_id,
        &results,
    )
    .await;
    log::info!("After-response lookups: write outcome={outcome}");
}

#[cfg(test)]
mod tests {
    use edgezero_adapter_fastly::delivery::DeliveryError;
    use edgezero_core::http::{Method, StatusCode};
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink as _};
    use trusted_server_core::ec::EcContext;
    use trusted_server_core::ec::finalize::ec_finalize_response;
    use trusted_server_core::ec::page_load::NavigationPermit;
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
    use crate::require_identity_graph;
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
            "should send telemetry and after-response lookups without the proxy marker"
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
    fn post_delivery_with_nothing_queued_makes_no_request() {
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

        clock.drive(post_delivery_work(
            None,
            &TelemetryQueue::default(),
            &settings_with_tinybird(),
            &client.http_client(),
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
            mirror_enabled: false,
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
        let identity = crate::app::build_identity_app(Arc::new(settings.clone()))
            .expect("should build identity app");
        let kv = require_identity_graph(&identity).expect("should open the local identity store");
        ec_context.set_kv_snapshot(futures::executor::block_on(
            kv.load_snapshot(&seeded_ec_id()),
        ));
        FinalizeState {
            identity,
            ec_state: EcFinalizeState {
                ec_context,
                use_finalize_kv: false,
                eids_cookie: None,
                sharedid: trusted_server_core::ec::sharedid::SharedIdRequest::default(),
            },
            after_response: None,
        }
    }

    /// [`returning_reader_state`] after finalize confirmed the reader's
    /// navigation, with the after-response pull-sync lookups the entry point
    /// captured for it.
    fn returning_reader_after_response_state(settings: &Settings) -> FinalizeState {
        let app = crate::app::build_state_from_settings(settings.clone())
            .expect("should build the app state");
        let mut state = returning_reader_state(settings);
        let navigation = edgezero_core::http::request_builder()
            .uri("https://test-publisher.example.com/article")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-mode", "navigate")
            .body(edgezero_core::body::Body::empty())
            .expect("should build the navigation");
        state.ec_state.ec_context.set_navigation_permit(
            NavigationPermit::classify(&navigation).expect("should permit a navigation"),
        );
        let kv =
            require_identity_graph(&app.identity).expect("should open the local identity store");
        let mut response = edgezero_core::http::response_builder()
            .status(StatusCode::OK)
            .body(edgezero_core::body::Body::empty())
            .expect("should build the page response");
        futures::executor::block_on(ec_finalize_response(
            settings,
            &mut state.ec_state.ec_context,
            Some(&kv),
            app.identity.partners(),
            None,
            &trusted_server_core::ec::sharedid::SharedIdRequest::default(),
            &mut response,
        ));
        state.identity = Arc::clone(&app.identity);
        state.after_response = Some(AfterResponseLookups {
            coordinator: Arc::clone(
                app.identity
                    .after_response_identity()
                    .expect("should run pull sync after the response"),
            ),
            ec_id: seeded_ec_id(),
            client_ip: None,
            user_agent: None,
            cookie_header: String::new(),
        });
        state
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
        let state = returning_reader_after_response_state(&settings);

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
