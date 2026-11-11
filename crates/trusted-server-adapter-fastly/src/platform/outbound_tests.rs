//! Shared TS collection policy under the real request-local Fastly progress driver.
//! Requires the disposable origin started by tests/deadline-origin.mjs.

use std::time::Duration;

use edgezero_adapter_fastly::delivery::{
    FastlyRequestDelivery, FastlyResponseCommitState, testing::response_delivery,
};
use edgezero_adapter_fastly::request::FastlyService;
use edgezero_core::app::App;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Response, request_builder};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use edgezero_core::router::RouterService;
use futures::executor::block_on;
use trusted_server_core::platform::{
    AuctionTargetId, EdgeZeroHttpClient, PlatformCacheIntent, PlatformHttpClient,
    PlatformHttpRequest, PlatformPendingRequest,
};
use web_time::Instant;

#[derive(Clone, Copy)]
enum Scenario {
    SlowOrigin,
    StalledHeaders,
    TrickleBody,
    Siblings,
    Head,
}

fn outbound(base: &str, path: &str, method: &str, backend: &str) -> PlatformHttpRequest {
    PlatformHttpRequest::new(
        request_builder()
            .method(method)
            .uri(format!("{base}/{path}"))
            .body(Body::empty())
            .expect("should build the fixture exchange"),
        backend,
    )
    .with_cache_bypass()
}

async fn pending(
    client: &dyn PlatformHttpClient,
    base: &str,
    path: &str,
    backend: &str,
) -> PlatformPendingRequest {
    client
        .send_async(outbound(base, path, "GET", backend))
        .await
        .expect("should schedule through the incoming Fastly client")
}

async fn exercise(context: RequestContext, scenario: Scenario) -> Result<Response, EdgeError> {
    let client = EdgeZeroHttpClient::for_request(&context, AuctionTargetId::Fastly);
    let base = fastly::ConfigStore::try_open("deadline_fixture")
        .expect("should open fixture inputs")
        .try_get("origin")
        .expect("should read the fixture origin")
        .expect("should supply a loopback origin");
    match scenario {
        Scenario::SlowOrigin => {
            let fast = pending(client.as_ref(), &base, "fast", "fast").await;
            let stalled = pending(client.as_ref(), &base, "stalled", "stalled").await;
            let deadline = Instant::now() + Duration::from_millis(40);
            // Wait for an actual publisher origin through the same SDK driver.
            // The driver can finish the bid while the application awaits that origin.
            client
                .send(outbound(&base, "slow-origin", "GET", "publisher"))
                .await
                .expect("should await the slow publisher origin");
            let start = Instant::now();
            let result = client
                .select_until(vec![stalled, fast], deadline)
                .await
                .expect("should take a non-driving completion snapshot")
                .expect("should retain a completed buffered bid");
            assert_eq!(
                result
                    .ready
                    .expect("should preserve the healthy response")
                    .backend_name
                    .as_deref(),
                Some("fast"),
                "should retain the completed provider"
            );
            assert!(
                result.remaining.is_empty(),
                "should cancel unfinished work at the cutoff"
            );
            assert!(
                start.elapsed() < Duration::from_millis(100),
                "should not wait for the stalled sibling after expiry"
            );
        }
        Scenario::StalledHeaders | Scenario::TrickleBody => {
            let path = if matches!(scenario, Scenario::StalledHeaders) {
                "stalled"
            } else {
                "trickle"
            };
            let request = pending(client.as_ref(), &base, path, "slow").await;
            let start = Instant::now();
            let result = client
                .select_until(vec![request], start + Duration::from_millis(80))
                .await
                .expect("should bound the full exchange wait");
            assert!(
                result.is_none(),
                "should not accept headers or partial bytes as a completed response"
            );
            assert!(
                start.elapsed() < Duration::from_millis(500),
                "should stop at the collection cutoff"
            );
        }
        Scenario::Siblings => {
            let slow = pending(client.as_ref(), &base, "trickle", "slow").await;
            let fast = pending(client.as_ref(), &base, "fast", "fast").await;
            let failed = pending(client.as_ref(), &base, "failed", "failed").await;
            let mut cached = outbound(&base, "fast", "GET", "default-cache");
            cached.cache_intent = PlatformCacheIntent::Default;
            let cached = client
                .send_async(cached)
                .await
                .expect("should schedule a default-cache exchange through the SDK");
            let deadline = Instant::now() + Duration::from_millis(300);
            let mut requests = vec![slow, fast, failed, cached];
            let mut successes = Vec::new();
            let mut errors = Vec::new();
            for _ in 0..3 {
                let result = client
                    .select_until(requests, deadline)
                    .await
                    .expect("should isolate completed siblings")
                    .expect("should finish before the deadline");
                match result.ready {
                    Ok(response) => {
                        assert_eq!(
                            response.response.body().as_bytes(),
                            Some(b"fast".as_slice()),
                            "should retain exact buffered bytes"
                        );
                        successes.push(
                            response
                                .backend_name
                                .expect("should retain successful provider correlation"),
                        );
                    }
                    Err(_) => errors.push(
                        result
                            .failed_backend_name
                            .expect("should attribute the failed provider"),
                    ),
                }
                requests = result.remaining;
            }
            successes.sort();
            assert_eq!(
                successes,
                ["default-cache", "fast"],
                "should retain bypass and SDK-default cache exchanges without starvation"
            );
            assert_eq!(
                errors,
                ["failed"],
                "should isolate the genuine transport error"
            );
            assert_eq!(
                requests.len(),
                1,
                "should retain only the incomplete sibling before the cutoff"
            );
        }
        Scenario::Head => {
            let request = client
                .send_async(outbound(&base, "head", "HEAD", "head"))
                .await
                .expect("should schedule HEAD");
            let result = client
                .select_until(vec![request], Instant::now() + Duration::from_secs(1))
                .await
                .expect("should collect HEAD")
                .expect("should finish before cutoff");
            let response = result
                .ready
                .expect("should accept advisory Content-Length above the body limit");
            assert_eq!(
                response.response.body().as_bytes(),
                Some(b"".as_slice()),
                "should not read a HEAD body"
            );
            let coded = client
                .send_async(outbound(&base, "gzip", "GET", "coded"))
                .await
                .expect("should schedule an original coded entity");
            let coded = client
                .select_until(vec![coded], Instant::now() + Duration::from_secs(1))
                .await
                .expect("should collect coded bytes")
                .expect("should complete coded response")
                .ready
                .expect("should retain the coded response");
            assert_eq!(
                coded.response.headers()["content-encoding"],
                "gzip",
                "should retain Fastly's original coding policy"
            );
            let body = coded
                .response
                .body()
                .as_bytes()
                .expect("should retain buffered encoded entity bytes");
            assert!(
                body.starts_with(&[31, 139, 8]),
                "should not replace encoded bytes with decoded text"
            );
            assert_eq!(
                body.len().to_string(),
                coded.response.headers()["content-length"]
                    .to_str()
                    .expect("should retain coded length"),
                "should account for original entity bytes"
            );
        }
    }
    Ok(Response::new(Body::from("okay")))
}

fn run(scenario: Scenario) {
    let router = RouterService::builder()
        .get("/", move |context: RequestContext| {
            exercise(context, scenario)
        })
        .build();
    let app = App::new(router);
    let result = FastlyService::new(&app).run_request_with_hooks_for_test(
        fastly::Request::get("http://publisher.example.com/"),
        |_raw, _extensions| {},
        |response| {
            assert_eq!(
                response.status().as_u16(),
                200,
                "should finish the fixture route"
            )
        },
        |envelope| {
            let (prepared, _, mut attempt, clock) = envelope
                .begin(ResponseEgressFramingMode::HostAutomatic)
                .map_err(|_| ())
                .expect("should begin fixture egress");
            assert!(attempt.begin_writing(), "should begin writing once");
            assert_eq!(
                block_on(
                    prepared
                        .into_response()
                        .into_body()
                        .into_bytes_bounded(1024)
                )
                .expect("should collect the fixture result")
                .as_ref(),
                b"okay",
                "should complete the production collection path"
            );
            assert!(
                attempt.complete(clock.now()),
                "should finish managed egress"
            );
            response_delivery(
                FastlyResponseCommitState::Committed,
                attempt.into_terminal_report(),
                Ok(()),
            )
        },
    );
    assert!(
        matches!(result, FastlyRequestDelivery::Routed { delivery, .. } if delivery.error().is_none()),
        "should retain successful managed dispatch"
    );
}

#[test]
#[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
fn network_slow_origin_preserves_ready_bid_without_waiting_for_stalled_sibling() {
    run(Scenario::SlowOrigin);
}

#[test]
#[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
fn network_stalled_headers_stop_at_deadline() {
    run(Scenario::StalledHeaders);
}

#[test]
#[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
fn network_trickling_body_stops_at_deadline() {
    run(Scenario::TrickleBody);
}

#[test]
#[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
fn network_fast_sibling_completes_while_another_body_trickles() {
    run(Scenario::Siblings);
}

#[test]
#[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
fn network_head_skips_the_body_and_its_declared_length_limit() {
    run(Scenario::Head);
}
