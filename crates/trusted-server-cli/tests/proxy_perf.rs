//! Explicitly selected local performance workloads for `ts dev proxy`.
//!
//! These tests assert deterministic connection and handshake counts. Their
//! wall-clock output is evidence for manual comparison, never a CI threshold.

#![cfg(target_os = "macos")]
#![allow(clippy::print_stdout)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt as _, stream};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

mod support;

const REQUEST_COUNT: usize = 100;

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_pooled_sequential_tls() {
    let variant = perf_variant();
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
        "PERF_RUN workload=sequential_tls variant={variant} run=1 duration_us={} \
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
    let expected_connections = if variant == "baseline" { 100 } else { 1 };
    assert_eq!(snapshot.accepted_connections, expected_connections);
    assert_eq!(snapshot.tls_handshakes, expected_connections);
    assert_eq!(
        snapshot.requests, REQUEST_COUNT as u64,
        "upstream should receive every request"
    );
    assert_eq!(snapshot.failures, 0, "baseline should have no failures");
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_pooled_matched_concurrency_six() {
    run_concurrent_pooled("matched_concurrency_6", 6).await;
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_pooled_saturation_concurrency_twenty() {
    run_concurrent_pooled("saturation_concurrency_20", 20).await;
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_http1_remote_model() {
    run_concurrent_pooled("http1_remote_model_30_30_25", 20).await;
}

async fn run_concurrent_pooled(workload: &str, concurrency: usize) {
    let variant = perf_variant();
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
        "PERF_RUN workload={workload} variant={variant} run=1 duration_us={} \
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
    if matches!(variant.as_str(), "baseline" | "remote_baseline") {
        assert_eq!(snapshot.accepted_connections, REQUEST_COUNT as u64);
    } else if variant == "cap20" {
        assert!(snapshot.accepted_connections < REQUEST_COUNT as u64);
    } else if concurrency > 6 {
        assert!(
            (2..=6).contains(&snapshot.accepted_connections),
            "queued saturation should keep total connections within the six-live cap, observed {}",
            snapshot.accepted_connections
        );
    } else {
        assert!(
            snapshot.accepted_connections < REQUEST_COUNT as u64,
            "matched concurrency should still reduce connection churn, observed {}",
            snapshot.accepted_connections
        );
    }
    assert_eq!(snapshot.tls_handshakes, snapshot.accepted_connections);
    assert_eq!(
        snapshot.requests, REQUEST_COUNT as u64,
        "upstream should receive every request"
    );
    assert_eq!(snapshot.failures, 0, "baseline should have no failures");
}

fn perf_variant() -> String {
    std::env::var("TS_PERF_VARIANT").unwrap_or_else(|_| "pooled".to_string())
}

fn numbered_paths() -> Vec<String> {
    (0..REQUEST_COUNT)
        .map(|index| format!("/asset-{index}"))
        .collect()
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_parser_local() {
    const CONNECTIONS: usize = 1_000;
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;
    let started = Instant::now();
    for _ in 0..CONNECTIONS {
        let mut stream = TcpStream::connect(proxy).await.expect("connect proxy");
        stream
            .write_all(b"GET /proxy.pac HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write PAC request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read PAC");
        assert!(response.starts_with(b"HTTP/1.1 200"));
    }
    let duration = started.elapsed();
    let snapshot = state.metrics.snapshot();
    let parse_us = snapshot.initial_head_parse_latency.total_micros();
    let ratio = parse_us as f64 / duration.as_micros() as f64;
    println!(
        "PERF_RUN workload=parser_local variant=byte_reader run=1 duration_us={} parse_us={} parse_ratio={ratio:.6}",
        duration.as_micros(),
        parse_us,
    );
    assert_eq!(snapshot.initial_heads_parsed, CONNECTIONS as u64);
}

#[tokio::test]
#[ignore = "manual performance workload"]
async fn perf_dns_lookup_contribution() {
    let variant = perf_variant();
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config_dns(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;
    let paths = numbered_paths();
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let started = Instant::now();
    let responses = support::drive_sequential_requests_through_proxy(proxy, &path_refs).await;
    let duration = started.elapsed();
    assert!(responses.iter().all(|response| response.status == 200));
    let snapshot = state.metrics.snapshot();
    let dns_us = snapshot.dns_lookup_latency.total_micros();
    let headers_us = snapshot.request_to_headers_latency.total_micros();
    let ratio = dns_us as f64 / headers_us.max(1) as f64;
    println!(
        "PERF_RUN workload=dns_lookup_contribution variant={variant} run=1 duration_us={} dns_us={} request_to_headers_us={} dns_ratio={ratio:.6} dns_hits={} dns_misses={}",
        duration.as_micros(),
        dns_us,
        headers_us,
        snapshot.dns_cache_hits,
        snapshot.dns_cache_misses,
    );
}
