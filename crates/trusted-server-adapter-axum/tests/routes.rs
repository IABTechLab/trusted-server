//! Integration tests for the Axum dev server.
//!
//! Uses `EdgeZeroAxumService` directly (no live TCP server) so tests remain fast
//! and self-contained. Each test builds the full `TrustedServerApp` router and
//! drives it through the Tower `Service` interface.

use axum::body::Body as AxumBody;
use axum::http::Request;
use edgezero_adapter_axum::EdgeZeroAxumService;
use edgezero_core::app::Hooks as _;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp;

fn make_service() -> EdgeZeroAxumService {
    EdgeZeroAxumService::new(TrustedServerApp::routes())
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify routing (not business logic correctness)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_is_routed() {
    // Verifies the route exists — 5xx from missing signing keys is acceptable;
    // 404 is not (that would mean the route was not registered).
    let mut svc = make_service();

    let req = Request::builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();

    assert_ne!(
        resp.status().as_u16(),
        404,
        "discovery endpoint must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_endpoint_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();

    assert_ne!(
        resp.status().as_u16(),
        404,
        "verify-signature must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();

    assert_ne!(
        resp.status().as_u16(),
        404,
        "admin/keys/rotate must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();

    assert_ne!(
        resp.status().as_u16(),
        404,
        "admin/keys/deactivate must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_returns_non_5xx() {
    // Config store write rejection must produce a structured 4xx, not 5xx/panic.
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();
    let status = resp.status().as_u16();

    assert_ne!(status, 404, "admin/keys/rotate must be routed");
    assert!(
        status < 500,
        "config store write rejection must produce 4xx, not 5xx: got {status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_prefix_is_handled_not_5xx() {
    let mut svc = make_service();

    // /static/tsjs= is a GET /{*rest} catch-all path. The handler returns 404
    // for an unknown hash, which is correct application behaviour (not a routing 404).
    // This test verifies the handler is reached (no 5xx/panic) and that routing works.
    let req = Request::builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
}

// ---------------------------------------------------------------------------
// Middleware tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_sets_geo_unavailable_header() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();

    assert_eq!(
        resp.headers()
            .get("x-geo-info-available")
            .and_then(|v| v.to_str().ok()),
        Some("false"),
        "finalize middleware should set X-Geo-Info-Available: false on every response"
    );
}

// ---------------------------------------------------------------------------
// Basic-auth gate test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_returns_non_404_non_5xx() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc.ready().await.unwrap().call(req).await.unwrap();
    let status = resp.status().as_u16();

    assert_ne!(status, 404, "admin route must be routed");
    assert!(
        status < 500,
        "admin route should not return 5xx: got {status}"
    );
}
