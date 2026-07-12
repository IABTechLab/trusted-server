//! End-to-end proxy tests: matched hosts are MITM'd and rewritten; unmatched
//! hosts on loopback are blind-tunnelled; injected Basic auth clears a gate; and
//! one keep-alive tunnel carries many sequential requests (spec §5/§8/§11/§14).
//!
//! Run with: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml
//!   --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e`

// The proxy under test is macOS-only (see `lib.rs`); skip this entire test crate
// on other targets so it does not reference the macOS-scoped dev-dependencies.
#![cfg(target_os = "macos")]

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use trusted_server_cli::commands::dev::proxy::{ca, config};

mod support;

async fn wait_for_request_metrics(
    state: &trusted_server_cli::commands::dev::proxy::ProxyState,
    completed: u64,
    failed: u64,
) {
    let settled = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = state.metrics.snapshot();
            if snapshot.requests_completed >= completed && snapshot.requests_failed >= failed {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "request metrics should settle: {:?}",
        state.metrics.snapshot()
    );
}

async fn wait_for_raw_connections(upstream: &support::RawUpstream, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while upstream.accepted_connections() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("raw upstream connections should settle");
}

#[tokio::test]
async fn matched_host_is_rewritten_and_forwarded() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let response = support::drive_request_through_proxy(cfg, ca).await;

    assert_eq!(response.status, 200, "response streamed back");
    assert_eq!(
        response.seen_host,
        support::FROM_HOST,
        "Host preserved as FROM"
    );
    assert_eq!(
        response.seen_orig_host,
        support::FROM_HOST,
        "X-Orig-Host is FROM"
    );
    assert_eq!(
        response.seen_forwarded_host,
        support::FROM_HOST,
        "X-Forwarded-Host is FROM"
    );
}

#[tokio::test]
async fn rewrite_host_keeps_forwarded_host_on_from() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config_rewrite_host(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let response = support::drive_request_through_proxy(cfg, ca).await;

    assert_eq!(response.status, 200, "response streamed back");
    assert_eq!(
        response.seen_host,
        upstream.addr.to_string(),
        "--rewrite-host sends Host: TO"
    );
    // The point: TS anchors URL rewriting to X-Forwarded-Host, so it stays FROM
    // even though Host is TO — keeping emitted first-party URLs on the prod host.
    assert_eq!(
        response.seen_forwarded_host,
        support::FROM_HOST,
        "X-Forwarded-Host stays FROM even with --rewrite-host"
    );
}

#[tokio::test]
async fn resolve_pins_connection_to_address() {
    let upstream = support::start_echo_upstream().await;
    // The TO host is `pinned.invalid` (never DNS-resolvable); `--resolve` sends
    // the connection to the real upstream, so a 200 proves the pin is honored.
    let cfg = support::test_config_with_resolve(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let response = support::drive_request_through_proxy(cfg, ca).await;

    assert_eq!(
        response.status, 200,
        "a non-resolvable TO host still reaches the upstream via --resolve"
    );
    assert_eq!(
        response.seen_host,
        support::FROM_HOST,
        "Host stays FROM (no --rewrite-host)"
    );
}

#[tokio::test]
async fn unmatched_host_is_blind_tunneled_on_loopback() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config_without_rules();
    let ca = Arc::new(support::dev_ca());

    let observed = support::connect_through_proxy_capturing_cert(
        cfg,
        ca,
        &upstream.addr,
        "upstream.localhost",
    )
    .await;

    assert_eq!(
        observed.issuer_common_name, "upstream.localhost",
        "blind tunnel presents the upstream cert"
    );
    assert_ne!(
        observed.issuer_common_name,
        ca::CA_COMMON_NAME,
        "proxy did not MITM an unmatched host"
    );
}

#[tokio::test]
async fn basic_auth_injected_when_absent_clears_401() {
    let upstream = support::start_gated_upstream().await;
    let mut cfg = support::test_config(&upstream.addr);
    cfg.basic_auth = Some(config::BasicAuth::new("dev", "secret").expect("should build auth"));
    let ca = Arc::new(support::dev_ca());

    let response = support::drive_request_through_proxy(cfg, ca).await;

    assert_eq!(response.status, 200, "injected Basic auth clears the 401");
}

#[tokio::test]
async fn authorization_does_not_persist_on_reused_connection() {
    let upstream = support::start_gated_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let responses = support::drive_authorized_then_absent(cfg, ca).await;

    assert_eq!(
        responses[0].status, 200,
        "first request carries authorization"
    );
    assert_eq!(
        responses[1].status, 401,
        "second request must not inherit it"
    );
    let snapshot = upstream.snapshot();
    assert_eq!(snapshot.accepted_connections, 1);
    assert_eq!(snapshot.requests, 2);
}

#[test]
fn insecure_mode_warns_before_startup_failure() {
    let occupied =
        std::net::TcpListener::bind("127.0.0.1:0").expect("should reserve loopback port");
    let listen = occupied
        .local_addr()
        .expect("should read occupied address")
        .to_string();
    let ca_dir = tempfile::tempdir().expect("should create temporary CA directory");
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "dev",
            "proxy",
            "--map",
            "www.example.com=127.0.0.1:443",
            "--listen",
            &listen,
            "--ca-dir",
            ca_dir.path().to_str().expect("should encode CA path"),
            "--insecure",
        ])
        .output()
        .expect("should run ts process");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "occupied port should fail startup"
    );
    assert!(
        stderr.contains("--insecure: upstream TLS verification is DISABLED"),
        "stderr should warn loudly before bind failure: {stderr}"
    );
}

#[tokio::test]
async fn keep_alive_serves_multiple_sequential_requests() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let responses = support::drive_sequential_requests(cfg, ca, &["/one", "/two"]).await;

    assert_eq!(responses.len(), 2, "both requests answered");
    assert!(
        responses.iter().all(|r| r.status == 200),
        "each request gets 200"
    );
    assert_eq!(responses[0].path, "/one", "first request");
    assert_eq!(
        responses[1].path, "/two",
        "second request reused the tunnel"
    );
    let snapshot = upstream.snapshot();
    assert_eq!(
        snapshot.accepted_connections, 1,
        "sequential mapped requests should reuse one upstream TCP connection"
    );
    assert_eq!(
        snapshot.tls_handshakes, 1,
        "sequential mapped requests should reuse one upstream TLS session"
    );
}

#[tokio::test]
async fn distinct_from_hosts_reuse_the_same_logical_to_pool() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config_shared_to(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;

    let first = support::drive_host_through_proxy(proxy, support::FROM_HOST, "/first").await;
    let second = support::drive_host_through_proxy(proxy, support::ALT_FROM_HOST, "/second").await;

    assert_eq!(first.seen_host, support::FROM_HOST);
    assert_eq!(first.seen_forwarded_host, support::FROM_HOST);
    assert_eq!(second.seen_host, support::ALT_FROM_HOST);
    assert_eq!(second.seen_forwarded_host, support::ALT_FROM_HOST);
    let snapshot = upstream.snapshot();
    assert_eq!(snapshot.accepted_connections, 1);
    assert_eq!(snapshot.requests, 2);
}

#[tokio::test]
async fn distinct_to_ports_never_share_upstream_connections() {
    let first_upstream = support::start_echo_upstream().await;
    let second_upstream = support::start_echo_upstream().await;
    let cfg = support::test_config_distinct_to(&first_upstream.addr, &second_upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;

    let first = support::drive_host_through_proxy(proxy, support::FROM_HOST, "/first").await;
    let second = support::drive_host_through_proxy(proxy, support::ALT_FROM_HOST, "/second").await;

    assert_eq!(first.path, "/first");
    assert_eq!(second.path, "/second");
    assert_eq!(first_upstream.snapshot().accepted_connections, 1);
    assert_eq!(second_upstream.snapshot().accepted_connections, 1);
}

#[tokio::test]
async fn upstream_connection_close_is_not_reused() {
    let upstream = support::start_closing_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let responses = support::drive_sequential_requests(cfg, ca, &["/one", "/two"]).await;

    assert!(responses.iter().all(|response| response.status == 200));
    let snapshot = upstream.snapshot();
    assert_eq!(snapshot.accepted_connections, 2);
    assert_eq!(snapshot.tls_handshakes, 2);
}

#[tokio::test]
async fn reused_empty_get_retries_once_after_post_dispatch_close() {
    let upstream = support::start_post_dispatch_stale_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let responses = support::drive_sequential_requests(cfg, ca, &["/one", "/two"]).await;

    assert!(responses.iter().all(|response| response.status == 200));
    let snapshot = upstream.snapshot();
    assert_eq!(
        snapshot.accepted_connections, 2,
        "retry should open one replacement"
    );
    assert_eq!(
        snapshot.requests, 3,
        "second GET should be attempted exactly twice"
    );
}

#[tokio::test]
async fn manager_shutdown_aborts_idle_driver_and_acknowledges() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;
    let responses = support::drive_sequential_requests_through_proxy(proxy, &["/idle"]).await;
    assert_eq!(responses[0].status, 200);

    tokio::time::timeout(Duration::from_secs(1), state.upstream.shutdown())
        .await
        .expect("manager should acknowledge after driver guard closes");
}

#[tokio::test]
async fn shutdown_waits_for_delayed_driver_reconciliation() {
    let manager = trusted_server_cli::commands::dev::proxy::upstream::manager::Manager::start(
        trusted_server_cli::commands::dev::proxy::upstream::manager::PoolLimits::default(),
    );
    let key = trusted_server_cli::commands::dev::proxy::upstream::key::OriginKey::new(
        trusted_server_cli::commands::dev::proxy::upstream::key::Transport::Tls,
        trusted_server_cli::commands::dev::proxy::upstream::key::ReferenceIdentity::dns(
            "shutdown.example",
        ),
        443,
        trusted_server_cli::commands::dev::proxy::upstream::key::VerifyMode::Secure,
        trusted_server_cli::commands::dev::proxy::upstream::key::AddressPolicy::Dns,
    );
    let trusted_server_cli::commands::dev::proxy::upstream::manager::Acquired::Open(reservation) =
        manager.acquire(key).await.expect("should reserve driver")
    else {
        panic!("should open driver reservation");
    };
    let id = reservation.id();
    let driver = tokio::spawn(std::future::pending::<()>());
    let lease = reservation.register(&manager, (), driver.abort_handle());
    manager.return_idle(lease.connection);
    tokio::task::yield_now().await;
    let shutdown = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.shutdown().await }
    });
    tokio::task::yield_now().await;

    assert!(
        driver.is_finished(),
        "shutdown should abort registered driver"
    );
    assert!(
        !shutdown.is_finished(),
        "shutdown must await delayed lifecycle reconciliation"
    );
    manager.driver_closed(id);
    tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .expect("reconciled shutdown should remain externally bounded")
        .expect("shutdown task should join");
}

#[tokio::test]
async fn post_dispatch_failure_does_not_retry_post() {
    let upstream = support::start_post_dispatch_stale_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let responses = support::drive_get_then_post(cfg, ca).await;

    assert_eq!(responses[0].status, 200);
    assert_eq!(responses[1].status, 502);
    let snapshot = upstream.snapshot();
    assert_eq!(
        snapshot.accepted_connections, 1,
        "POST must not open retry connection"
    );
    assert_eq!(snapshot.requests, 2, "POST must be attempted exactly once");
}

#[tokio::test]
async fn large_chunked_upload_and_response_are_byte_identical() {
    let upstream = support::start_chunked_body_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let body: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|index| ((index * 31 + 17) % 251) as u8)
        .collect();

    let response = support::drive_chunked_body(cfg, ca, &body).await;

    assert_eq!(response, body, "chunked body should remain byte-identical");
    assert_eq!(
        upstream.snapshot(),
        support::UpstreamSnapshot {
            accepted_connections: 1,
            tls_handshakes: 1,
            requests: 1,
            failures: 0,
        },
        "one streamed exchange should use one healthy upstream connection"
    );
}

#[tokio::test]
async fn early_response_completes_without_pooling_streaming_upload() {
    let upstream = support::start_early_response_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;

    let status = support::drive_early_response(proxy).await;
    assert_eq!(status, 200, "browser should receive early response");
    wait_for_request_metrics(&state, 1, 0).await;
    let next = support::drive_sequential_requests_through_proxy(proxy, &["/next"]).await;

    assert_eq!(next[0].status, 200);
    assert_eq!(
        upstream.snapshot().accepted_connections,
        2,
        "streaming upload connection must close instead of pooling"
    );
    let metrics = state.metrics.snapshot();
    assert_eq!(metrics.requests_completed, 2);
    assert_eq!(metrics.requests_failed, 0);
}

#[tokio::test]
async fn browser_cancellation_closes_slow_response_and_counts_failure() {
    let upstream = support::start_slow_response_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;

    support::cancel_slow_response(proxy).await;
    wait_for_request_metrics(&state, 0, 1).await;

    tokio::time::timeout(Duration::from_secs(1), state.upstream.shutdown())
        .await
        .expect("cancelled response driver should reconcile");
    assert_eq!(state.metrics.snapshot().requests_completed, 0);
}

#[tokio::test]
async fn truncated_upstream_response_counts_failure() {
    let upstream = support::start_truncated_response_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;

    let (declared, received) = support::drive_truncated_response(proxy).await;
    wait_for_request_metrics(&state, 0, 1).await;

    assert_eq!(declared, 10);
    assert_eq!(received, 3);
    assert_eq!(state.metrics.snapshot().requests_completed, 0);
}

#[tokio::test]
async fn response_trailers_finish_before_connection_reuse() {
    let upstream = support::start_trailer_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;

    let responses = support::drive_trailer_requests(proxy).await;
    wait_for_request_metrics(&state, 2, 0).await;

    assert_eq!(responses.len(), 2);
    for (body, trailers) in responses {
        assert_eq!(body, b"data");
        assert!(
            trailers
                .to_ascii_lowercase()
                .contains("x-test-trailer: done"),
            "response trailer should be forwarded: {trailers:?}"
        );
    }
    let snapshot = upstream.snapshot();
    assert_eq!(snapshot.accepted_connections, 1);
    assert_eq!(snapshot.requests, 2);
}

#[tokio::test]
async fn mismatched_host_over_mitm_tunnel_is_refused_with_421() {
    // CONNECT a mapped host (so the tunnel is MITM'd), then send a request whose
    // Host header matches NO rule. It must be refused with 421 (Misdirected
    // Request), never rerouted through the CONNECT-authority rule — otherwise a
    // client could CONNECT a mapped host and smuggle traffic for any other host
    // through that rule (spec §8.2).
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());

    let status = support::drive_request_with_host_header(cfg, ca, "unmapped.example.com").await;

    assert_eq!(
        status, 421,
        "a Host that matches no rule must be refused with 421, not rerouted"
    );
}

#[tokio::test]
async fn unmatched_connect_off_loopback_is_refused_with_403() {
    // The proxy is set up with no rule matching "unmapped.example.com", and the
    // server is made to believe it is bound on a non-loopback interface.  An
    // unmatched CONNECT must be refused with 403, never blind-tunnelled (spec §5).
    let cfg = support::test_config_without_rules();
    let ca = Arc::new(support::dev_ca());

    let proxy = support::spawn_proxy_as_non_loopback(cfg, ca).await;
    let status = support::connect_and_read_status(proxy, "unmapped.example.com:443").await;

    assert!(
        status.contains(" 403 "),
        "off-loopback unmatched CONNECT must be refused with 403, got: {status}"
    );
}

#[tokio::test]
async fn blind_and_plain_forwarding_bypass_mapped_pool_capacity() {
    let mapped = support::start_echo_upstream().await;
    let raw = support::start_raw_echo_upstream().await;
    let cfg = support::test_config(&mapped.addr);
    let ca = Arc::new(support::dev_ca());
    let (proxy, state) = support::spawn_proxy_with_state(cfg, ca).await;
    let authority = raw.addr.to_string();

    let mut blind = Vec::new();
    for _ in 0..65 {
        blind.push(support::open_blind_tunnel(proxy, &authority, &[]).await);
    }
    wait_for_raw_connections(&raw, 65).await;
    let first = support::drive_sequential_requests_through_proxy(proxy, &["/after-blind"]).await;
    assert_eq!(first[0].status, 200);
    drop(blind);

    let mut plain = Vec::new();
    for _ in 0..65 {
        plain.push(support::open_plain_forward(proxy, raw.addr, b"").await.0);
    }
    wait_for_raw_connections(&raw, 130).await;
    let second = support::drive_sequential_requests_through_proxy(proxy, &["/after-plain"]).await;
    assert_eq!(second[0].status, 200);
    drop(plain);

    tokio::time::timeout(Duration::from_secs(1), state.upstream.shutdown())
        .await
        .expect("forwarding sockets should not block mapped manager shutdown");
}

#[tokio::test]
async fn blind_tunnel_overread_reaches_upstream_exactly_once() {
    let mapped = support::start_echo_upstream().await;
    let raw = support::start_raw_echo_upstream().await;
    let cfg = support::test_config(&mapped.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;
    let payload = b"blind-overread-prefix";

    let mut tunnel = support::open_blind_tunnel(proxy, &raw.addr.to_string(), payload).await;
    let mut echoed = vec![0_u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(1), tunnel.read_exact(&mut echoed))
        .await
        .expect("blind overread should be echoed")
        .expect("should read blind overread");

    assert_eq!(echoed, payload);
    wait_for_raw_connections(&raw, 1).await;
    assert_eq!(raw.captured()[0], payload);
}

#[tokio::test]
async fn plain_forward_overread_reaches_upstream_exactly_once() {
    let mapped = support::start_echo_upstream().await;
    let raw = support::start_raw_echo_upstream().await;
    let cfg = support::test_config(&mapped.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;

    let (_client, expected) =
        support::open_plain_forward(proxy, raw.addr, b"plain-overread-body").await;
    wait_for_raw_connections(&raw, 1).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while raw.captured()[0].len() < expected.len() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("plain overread should reach raw upstream");

    assert_eq!(raw.captured()[0], expected);
}

#[tokio::test]
async fn mitm_connect_overread_preserves_tls_client_hello() {
    let upstream = support::start_echo_upstream().await;
    let cfg = support::test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    let proxy = support::spawn_proxy(cfg, ca).await;

    let response = support::drive_mitm_connect_overread(proxy).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.path, "/mitm-overread");
}
