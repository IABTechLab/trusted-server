//! End-to-end proxy tests: matched hosts are MITM'd and rewritten; unmatched
//! hosts on loopback are blind-tunnelled; injected Basic auth clears a gate; and
//! one keep-alive tunnel carries many sequential requests (spec §5/§8/§11/§14).
//!
//! Run with: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml
//!   --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e`

// The proxy under test is macOS-only (see `lib.rs`); skip this entire test crate
// on other targets so it does not reference the macOS-scoped dev-dependencies.
#![cfg(target_os = "macos")]

use std::sync::Arc;

use trusted_server_cli::commands::dev::proxy::{ca, config};

mod support;

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
