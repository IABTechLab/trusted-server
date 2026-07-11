//! Explicitly selected local performance workloads for `ts dev proxy`.
//!
//! These tests assert deterministic connection and handshake counts. Their
//! wall-clock output is evidence for manual comparison, never a CI threshold.

#![cfg(target_os = "macos")]
#![allow(clippy::print_stdout)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt as _, stream};

mod support;

const REQUEST_COUNT: usize = 100;

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_baseline_sequential_tls() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;
    let paths = numbered_paths();
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let started = Instant::now();
    let responses = support::drive_sequential_requests_through_proxy(proxy, &path_refs).await;
    let duration = started.elapsed();
    let snapshot = upstream.snapshot();

    println!(
        "PERF_RUN workload=sequential_tls variant=baseline run=1 duration_us={} \
         tcp_attempts={} tcp_established={} tls_handshakes={} failures={}",
        duration.as_micros(),
        snapshot.accepted_connections,
        snapshot.accepted_connections,
        snapshot.tls_handshakes,
        snapshot.failures,
    );
    assert_eq!(
        responses.len(),
        REQUEST_COUNT,
        "should answer every request"
    );
    assert!(
        responses.iter().all(|response| response.status == 200),
        "should return success for every request"
    );
    assert_eq!(
        snapshot.accepted_connections, REQUEST_COUNT as u64,
        "baseline should open one upstream connection per request"
    );
    assert_eq!(
        snapshot.tls_handshakes, REQUEST_COUNT as u64,
        "baseline should negotiate TLS once per request"
    );
    assert_eq!(
        snapshot.requests, REQUEST_COUNT as u64,
        "upstream should receive every request"
    );
    assert_eq!(snapshot.failures, 0, "baseline should have no failures");
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_baseline_matched_concurrency_six() {
    run_concurrent_baseline("matched_concurrency_6", 6).await;
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_baseline_saturation_concurrency_twenty() {
    run_concurrent_baseline("saturation_concurrency_20", 20).await;
}

async fn run_concurrent_baseline(workload: &str, concurrency: usize) {
    let upstream = support::start_delayed_echo_upstream(Duration::from_millis(25)).await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;

    let started = Instant::now();
    let responses: Vec<support::ProxiedResponse> = stream::iter(0..REQUEST_COUNT)
        .map(|index| async move {
            let path = format!("/asset-{index}");
            support::drive_sequential_requests_through_proxy(proxy, &[path.as_str()])
                .await
                .into_iter()
                .next()
                .expect("should receive one response")
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let duration = started.elapsed();
    let snapshot = upstream.snapshot();

    println!(
        "PERF_RUN workload={workload} variant=baseline run=1 duration_us={} \
         tcp_attempts={} tcp_established={} tls_handshakes={} failures={}",
        duration.as_micros(),
        snapshot.accepted_connections,
        snapshot.accepted_connections,
        snapshot.tls_handshakes,
        snapshot.failures,
    );
    assert_eq!(
        responses.len(),
        REQUEST_COUNT,
        "should answer every request"
    );
    assert!(
        responses.iter().all(|response| response.status == 200),
        "should return success for every request"
    );
    assert_eq!(
        snapshot.accepted_connections, REQUEST_COUNT as u64,
        "baseline should open one upstream connection per request"
    );
    assert_eq!(
        snapshot.tls_handshakes, REQUEST_COUNT as u64,
        "baseline should negotiate TLS once per request"
    );
    assert_eq!(
        snapshot.requests, REQUEST_COUNT as u64,
        "upstream should receive every request"
    );
    assert_eq!(snapshot.failures, 0, "baseline should have no failures");
}

fn numbered_paths() -> Vec<String> {
    (0..REQUEST_COUNT)
        .map(|index| format!("/asset-{index}"))
        .collect()
}
