//! End-to-end proxy tests: matched hosts are MITM'd and rewritten; unmatched
//! hosts on loopback are blind-tunnelled; injected Basic auth clears a gate; and
//! one keep-alive tunnel carries many sequential requests (spec §5/§8/§11/§14).
//!
//! Run with: cargo test --manifest-path crates/trusted-server-cli/Cargo.toml \
//!   --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e

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
    cfg.basic_auth = Some(config::BasicAuth {
        user: "dev".into(),
        pass: "secret".into(),
    });
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
}
